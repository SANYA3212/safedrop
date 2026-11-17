package com.safedrop.app

import android.app.ActivityManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.Process
import androidx.core.content.ContextCompat
import org.json.JSONObject
import java.io.BufferedReader
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStreamReader
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.security.KeyFactory
import java.security.KeyStore
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.PKCS8EncodedKeySpec
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509TrustManager

// «SafeDrop на связи» — служба в своём процессе (`:watch` в манифесте). Её
// процесс Android не убивает, когда SafeDrop смахивают из недавних (особенно
// Xiaomi), а сам SafeDrop при этом закрывается целиком. Поэтому здесь
// маленький «сторож», как пуш у Telegram, только без Google: лёгкое
// соединение с сервером SafeDrop с ключом аккаунта, которое ждёт одно —
// «тебе звонят» (`ring`). Тогда звонок на весь экран (CallNotifier), а
// «Ответить» запускает SafeDrop: он подключается, звонящий досылает ему
// приглашение, и звонок принимается. «Отклонить» — отказ через сервер.
class KeepAliveService : Service() {
  companion object {
    const val CHANNEL = "safedrop_online"
    const val ACTION_QUIT = "com.safedrop.app.QUIT"
    private const val NOTIFICATION_ID = 4712

    @JvmStatic
    fun start(ctx: Context) {
      try {
        ContextCompat.startForegroundService(ctx, Intent(ctx, KeepAliveService::class.java))
      } catch (e: Exception) {
        // Android 12+ не даёт запускать службу из фона — запустится при открытии
      }
    }

    @JvmStatic
    fun stop(ctx: Context) {
      ctx.stopService(Intent(ctx, KeepAliveService::class.java))
    }
  }

  private var watcher: RingWatcher? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_QUIT) {
      // «Выключить»: сторож, служба и сам SafeDrop — до следующего запуска
      watcher?.stop()
      watcher = null
      stopForeground(STOP_FOREGROUND_REMOVE)
      stopSelf()
      RingWatcher.killMainProcess(this)
      return START_NOT_STICKY
    }
    val nm = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26 && nm.getNotificationChannel(CHANNEL) == null) {
      val ch = NotificationChannel(CHANNEL, "Работа в фоне", NotificationManager.IMPORTANCE_MIN)
      ch.description = "SafeDrop на связи, даже когда окно закрыто"
      ch.setShowBadge(false)
      nm.createNotificationChannel(ch)
    }
    val open = PendingIntent.getActivity(
      this, 0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val quit = PendingIntent.getService(
      this, 1, Intent(this, KeepAliveService::class.java).setAction(ACTION_QUIT),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(this)
    val n = builder
      .setContentTitle("SafeDrop на связи")
      .setContentText("Звонки приходят, даже когда приложение закрыто")
      .setSmallIcon(R.drawable.ic_notification)
      .setOngoing(true)
      .setContentIntent(open)
      .addAction(Notification.Action.Builder(null, "Выключить", quit).build())
      .build()
    try {
      if (Build.VERSION.SDK_INT >= 34) {
        startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING)
      } else {
        startForeground(NOTIFICATION_ID, n)
      }
    } catch (e: Exception) {
      stopSelf()
      return START_NOT_STICKY
    }
    if (watcher == null) {
      watcher = RingWatcher(applicationContext).also { it.start() }
    }
    return START_STICKY
  }

  override fun onDestroy() {
    watcher?.stop()
    watcher = null
    super.onDestroy()
  }
}

/** Сторож: соединение с сервером SafeDrop, которое ждёт только звонков. */
class RingWatcher(private val ctx: Context) {
  companion object {
    @Volatile var current: RingWatcher? = null
    private const val PROTO = 1
    private const val DEFAULT_PORT = 47840
    /** сервер шлёт сторожу pong раз в 50 с; без единой строки дольше — связь мертва */
    private const val READ_TIMEOUT_MS = 200_000
    private const val PING_MS = 25_000L

    /** Отказ от звонка из уведомления (CallActionReceiver). */
    @JvmStatic
    fun decline(ip: String): Boolean = current?.send(JSONObject().put("t", "ring").put("to", ip.removePrefix("sd-")).put("state", "decline")) ?: false

    /** Процесс самого SafeDrop на экране — тогда звонит он сам. */
    fun mainOnScreen(ctx: Context): Boolean {
      val am = ctx.getSystemService(ActivityManager::class.java) ?: return false
      return am.runningAppProcesses?.any {
        it.processName == ctx.packageName && it.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
      } ?: false
    }

    fun killMainProcess(ctx: Context) {
      val am = ctx.getSystemService(ActivityManager::class.java) ?: return
      am.runningAppProcesses?.filter { it.processName == ctx.packageName }?.forEach { Process.killProcess(it.pid) }
    }
  }

  @Volatile private var running = false
  @Volatile private var out: OutputStream? = null
  @Volatile private var lastPing = 0L
  private var thread: Thread? = null
  private var ringingFrom = ""
  private var ringingName = ""

  fun start() {
    running = true
    current = this
    thread = Thread({ loop() }, "SafeDropRingWatch").also { it.isDaemon = true; it.start() }
  }

  fun stop() {
    running = false
    if (current === this) current = null
    try { out?.close() } catch (e: Exception) {}
    thread?.interrupt()
  }

  @Synchronized
  fun send(obj: JSONObject): Boolean {
    val o = out ?: return false
    return try {
      o.write((obj.toString() + "\n").toByteArray(Charsets.UTF_8))
      o.flush()
      if (obj.optString("t") == "ping") lastPing = System.currentTimeMillis()
      true
    } catch (e: Exception) {
      false
    }
  }

  private fun loop() {
    var backoff = 5_000L
    while (running) {
      val started = System.currentTimeMillis()
      try {
        session()
      } catch (e: InterruptedException) {
        return
      } catch (e: Exception) {
        // связь оборвалась — снова через паузу
      }
      out = null
      if (!running) return
      if (System.currentTimeMillis() - started > 120_000) backoff = 5_000L
      try { Thread.sleep(backoff) } catch (e: InterruptedException) { return }
      backoff = minOf(backoff * 2, 120_000L)
    }
  }

  /** Своё «устройство» сторожа (не то же, что у самого SafeDrop). */
  private fun watchDev(): String {
    val prefs = ctx.getSharedPreferences("safedrop_watch", Context.MODE_PRIVATE)
    prefs.getString("dev", null)?.let { return it }
    val dev = java.util.UUID.randomUUID().toString().replace("-", "").take(16)
    prefs.edit().putString("dev", dev).apply()
    return dev
  }

  private fun session() {
    val dir = ctx.filesDir
    val conf = try { JSONObject(File(dir, "server.json").readText()) } catch (e: Exception) { null }
    val certFile = File(dir, "quic_cert.der")
    val keyFile = File(dir, "quic_key.der")
    // не через сервер, не входили или ключ сервера ещё не запомнен — ждать
    if (conf == null || conf.optString("mode") != "server" || conf.optString("fp").isEmpty() || !certFile.isFile || !keyFile.isFile) {
      Thread.sleep(60_000)
      return
    }
    val (host, port) = splitAddr(conf.optString("addr")) ?: run { Thread.sleep(60_000); return }
    val tls = sslContext(certFile.readBytes(), keyFile.readBytes(), conf.optString("fp"))
    val raw = Socket()
    raw.connect(InetSocketAddress(host, port), 10_000)
    raw.soTimeout = READ_TIMEOUT_MS
    raw.tcpNoDelay = true
    val sock = tls.socketFactory.createSocket(raw, host, port, true) as SSLSocket
    sock.enabledProtocols = arrayOf("TLSv1.3")
    sock.startHandshake()
    sock.use { s ->
      out = s.outputStream
      send(
        JSONObject()
          .put("t", "hello").put("proto", PROTO).put("pass", conf.optString("pass"))
          .put("name", "").put("ver", "watch").put("dev", watchDev())
          .put("form", "phone").put("quiet", true).put("watch", true)
      )
      // свой ping — пока телефон не спит; во сне будит pong от сервера
      val pinger = Thread({
        try {
          while (running) {
            Thread.sleep(PING_MS)
            if (!send(JSONObject().put("t", "ping"))) break
          }
        } catch (e: InterruptedException) {}
      }, "SafeDropRingPing").also { it.isDaemon = true; it.start() }
      try {
        val r = BufferedReader(InputStreamReader(s.inputStream, Charsets.UTF_8))
        while (running) {
          val line = r.readLine() ?: break
          if (line.length > 64 * 1024) continue
          val m = try { JSONObject(line) } catch (e: Exception) { continue }
          when (m.optString("t")) {
            "deny" -> { Thread.sleep(600_000); return }
            "pong" -> if (System.currentTimeMillis() - lastPing > 40_000) send(JSONObject().put("t", "ping"))
            "ring" -> onRing(m.optString("from"), m.optString("name"), m.optString("state"))
          }
        }
      } finally {
        pinger.interrupt()
      }
    }
  }

  private fun onRing(from: String, name: String, state: String) {
    if (from.length != 64 || !from.all { it in '0'..'9' || it in 'a'..'f' }) return
    val ip = "sd-$from"
    val who = name.filter { !it.isISOControl() }.take(64).ifBlank { "SafeDrop" }
    when (state) {
      "" -> {
        // SafeDrop на экране — звонит сам; иначе — уведомление на весь экран
        if (mainOnScreen(ctx)) return
        ringingFrom = from
        ringingName = who
        CallNotifier.show(ctx, ip, who, "Входящий звонок")
      }
      "stop" -> if (ringingFrom == from) {
        ringingFrom = ""
        CallNotifier.missed(ctx, "Пропущенный звонок", ringingName)
      }
    }
  }

  private fun splitAddr(s: String): Pair<String, Int>? {
    val a = s.trim()
    if (a.isEmpty()) return null
    if (a.startsWith("[")) {
      val end = a.indexOf(']')
      if (end < 0) return null
      val host = a.substring(1, end)
      val tail = a.substring(end + 1)
      val port = if (tail.startsWith(":")) tail.substring(1).toIntOrNull() ?: return null else DEFAULT_PORT
      return host to port
    }
    val colon = a.lastIndexOf(':')
    if (colon > 0 && a.indexOf(':') == colon) {
      val port = a.substring(colon + 1).toIntOrNull() ?: return null
      return a.substring(0, colon) to port
    }
    return a to DEFAULT_PORT
  }

  /** TLS с ключом аккаунта; ключ сервера — только запомненный (как у SafeDrop). */
  private fun sslContext(certDer: ByteArray, keyDer: ByteArray, pinnedFp: String): SSLContext {
    val cert = CertificateFactory.getInstance("X.509").generateCertificate(ByteArrayInputStream(certDer)) as X509Certificate
    val key = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(keyDer))
    val ks = KeyStore.getInstance("PKCS12")
    ks.load(null, null)
    ks.setKeyEntry("me", key, CharArray(0), arrayOf(cert))
    val kmf = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm())
    kmf.init(ks, CharArray(0))
    val pin = object : X509TrustManager {
      override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        throw CertificateException("not a server")
      }
      override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        val first = chain?.firstOrNull() ?: throw CertificateException("no key")
        val fp = MessageDigest.getInstance("SHA-256").digest(first.encoded).joinToString("") { "%02x".format(it) }
        if (fp != pinnedFp) throw CertificateException("server key changed")
      }
      override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
    }
    val c = SSLContext.getInstance("TLSv1.3")
    c.init(kmf.keyManagers, arrayOf(pin), null)
    return c
  }
}
