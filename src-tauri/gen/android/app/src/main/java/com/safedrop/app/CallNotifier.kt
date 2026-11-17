package com.safedrop.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager

// Входящий звонок на телефоне. Звонит сам Android, как у Telegram: мелодия
// вызова телефона и вибрация (Ringer) — и когда SafeDrop открыт, и когда
// закрыт. Пока окна SafeDrop не видно (свёрнуто, закрыто, экран
// заблокирован), ещё и уведомление на весь экран: «Ответить» открывает
// SafeDrop и принимает звонок, «Отклонить» сбрасывает. Само уведомление
// беззвучное — звук только от Ringer (каналы уведомлений Xiaomi часто глушит).
// Вызывает Rust (mobile.rs) через JNI и сторож (KeepAliveService.kt).
object CallNotifier {
  const val CHANNEL = "safedrop_calls_v2"
  private const val OLD_CHANNEL = "safedrop_calls_v1"
  const val MISSED_CHANNEL = "safedrop_missed"
  const val EXTRA_IP = "sd_call_ip"
  const val EXTRA_ANSWER = "sd_call_answer"
  const val ACTION_DECLINE = "com.safedrop.app.DECLINE"
  const val ACTION_STOP_RING = "com.safedrop.app.STOP_RING"
  private const val NOTIFICATION_ID = 4713
  private const val MISSED_ID = 4714

  private fun channels(ctx: Context) {
    if (Build.VERSION.SDK_INT < 26) return
    val nm = ctx.getSystemService(NotificationManager::class.java)
    if (nm.getNotificationChannel(OLD_CHANNEL) != null) nm.deleteNotificationChannel(OLD_CHANNEL)
    if (nm.getNotificationChannel(CHANNEL) == null) {
      val ch = NotificationChannel(CHANNEL, "Входящие звонки", NotificationManager.IMPORTANCE_HIGH)
      ch.description = "Звонок на весь экран, когда SafeDrop закрыт или экран заблокирован"
      ch.setSound(null, null)
      ch.enableVibration(false)
      ch.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
      nm.createNotificationChannel(ch)
    }
    if (nm.getNotificationChannel(MISSED_CHANNEL) == null) {
      nm.createNotificationChannel(NotificationChannel(MISSED_CHANNEL, "Пропущенные звонки", NotificationManager.IMPORTANCE_DEFAULT))
    }
  }

  /** Звонок: звонит всегда; уведомление — только если окна SafeDrop не видно. */
  @JvmStatic
  fun show(ctx: Context, ip: String, title: String, text: String) {
    Ringer.start(ctx)
    if (MainActivity.onScreen) return
    channels(ctx)
    val open = Intent(ctx, MainActivity::class.java)
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
      .putExtra(EXTRA_IP, ip)
    val answer = Intent(open).putExtra(EXTRA_ANSWER, true)
    val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    val openPi = PendingIntent.getActivity(ctx, 10, open, flags)
    val answerPi = PendingIntent.getActivity(ctx, 11, answer, flags)
    val declinePi = PendingIntent.getBroadcast(
      ctx, 12,
      Intent(ctx, CallActionReceiver::class.java).setAction(ACTION_DECLINE).putExtra(EXTRA_IP, ip),
      flags
    )
    val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(ctx, CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(ctx)
    b.setContentTitle(title)
      .setContentText(text)
      .setSmallIcon(R.drawable.ic_notification)
      .setCategory(Notification.CATEGORY_CALL)
      .setOngoing(true)
      .setAutoCancel(true)
      .setVisibility(Notification.VISIBILITY_PUBLIC)
      .setContentIntent(openPi)
      .setFullScreenIntent(openPi, true)
      .addAction(Notification.Action.Builder(null, "Отклонить", declinePi).build())
      .addAction(Notification.Action.Builder(null, "Ответить", answerPi).build())
    if (Build.VERSION.SDK_INT >= 26) b.setTimeoutAfter(60_000)
    if (Build.VERSION.SDK_INT < 26) {
      @Suppress("DEPRECATION")
      b.setPriority(Notification.PRIORITY_MAX)
    }
    try {
      ctx.getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, b.build())
    } catch (e: SecurityException) {
      // нет разрешения на уведомления — звонит хотя бы Ringer
    }
  }

  /** Звонок кончился (ответили, отклонили, сбросили): тихо — здесь и у сторожа. */
  @JvmStatic
  fun cancel(ctx: Context) {
    Ringer.stop(ctx)
    ctx.getSystemService(NotificationManager::class.java).cancel(NOTIFICATION_ID)
    // сторож (`:watch`) мог звонить в своём процессе
    try {
      ctx.sendBroadcast(Intent(ctx, CallActionReceiver::class.java).setAction(ACTION_STOP_RING))
    } catch (e: Exception) {}
  }

  @JvmStatic
  fun missed(ctx: Context, title: String, text: String) {
    cancel(ctx)
    if (MainActivity.onScreen) return
    channels(ctx)
    val open = PendingIntent.getActivity(
      ctx, 13,
      Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(ctx, MISSED_CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(ctx)
    val n = b.setContentTitle(title)
      .setContentText(text)
      .setSmallIcon(R.drawable.ic_notification)
      .setCategory(Notification.CATEGORY_MISSED_CALL)
      .setAutoCancel(true)
      .setContentIntent(open)
      .build()
    try {
      ctx.getSystemService(NotificationManager::class.java).notify(MISSED_ID, n)
    } catch (e: SecurityException) {}
  }
}

/** Мелодия вызова телефона и вибрация — пока звонок ждёт ответа (не дольше минуты). */
object Ringer {
  private var ringtone: Ringtone? = null
  private var vibrator: Vibrator? = null
  private val main = Handler(Looper.getMainLooper())
  private val timeout = Runnable { stopNow() }

  @JvmStatic
  @Synchronized
  fun start(ctx: Context) {
    stopNow()
    val am = ctx.getSystemService(AudioManager::class.java)
    val mode = am?.ringerMode ?: AudioManager.RINGER_MODE_NORMAL
    // режим «Без звука» уважаем, «Вибрация» — только вибрируем
    if (mode == AudioManager.RINGER_MODE_NORMAL) {
      try {
        val uri = RingtoneManager.getActualDefaultRingtoneUri(ctx, RingtoneManager.TYPE_RINGTONE)
          ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
        val r = RingtoneManager.getRingtone(ctx.applicationContext, uri)
        if (r != null) {
          r.audioAttributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()
          if (Build.VERSION.SDK_INT >= 28) r.isLooping = true
          r.play()
          ringtone = r
        }
      } catch (e: Exception) {}
    }
    if (mode != AudioManager.RINGER_MODE_SILENT) {
      try {
        val v = if (Build.VERSION.SDK_INT >= 31) {
          ctx.getSystemService(VibratorManager::class.java)?.defaultVibrator
        } else {
          @Suppress("DEPRECATION") ctx.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
        }
        val pattern = longArrayOf(0, 800, 600, 800, 1400)
        if (v != null && v.hasVibrator()) {
          if (Build.VERSION.SDK_INT >= 26) v.vibrate(VibrationEffect.createWaveform(pattern, 0))
          else @Suppress("DEPRECATION") v.vibrate(pattern, 0)
          vibrator = v
        }
      } catch (e: Exception) {}
    }
    main.postDelayed(timeout, 60_000)
  }

  @JvmStatic
  fun stop(ctx: Context) = stopNow()

  @Synchronized
  private fun stopNow() {
    main.removeCallbacks(timeout)
    try { ringtone?.stop() } catch (e: Exception) {}
    ringtone = null
    try { vibrator?.cancel() } catch (e: Exception) {}
    vibrator = null
  }
}

/// «Отклонить» в уведомлении звонка и «замолчать» от процесса SafeDrop.
/// Работает в процессе сторожа (`:watch`): отказ уходит звонящему через
/// сервер, даже если SafeDrop закрыт.
class CallActionReceiver : BroadcastReceiver() {
  override fun onReceive(ctx: Context, intent: Intent) {
    when (intent.action) {
      CallNotifier.ACTION_STOP_RING -> Ringer.stop(ctx)
      CallNotifier.ACTION_DECLINE -> {
        Ringer.stop(ctx)
        CallNotifier.cancel(ctx)
        val ip = intent.getStringExtra(CallNotifier.EXTRA_IP) ?: return
        val done = goAsync()
        Thread {
          try { RingWatcher.decline(ip) } catch (e: Throwable) {}
          done.finish()
        }.start()
      }
    }
  }
}
