package com.safedrop.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.SystemClock
import java.io.ByteArrayOutputStream

// Демонстрация экрана с телефона. WebView не умеет захватывать экран, поэтому
// кадры снимает Android (MediaProjection) в этой службе, сжимает в JPEG и
// отдаёт в Rust (nativeFrame, mobile.rs); страница забирает их, рисует на
// холсте и шлёт как обычную демонстрацию. Android требует, чтобы запись экрана
// шла в службе с уведомлением — оно же кнопка «Остановить».
class ScreenCaptureService : Service() {
  companion object {
    const val EXTRA_CODE = "code"
    const val EXTRA_DATA = "data"
    const val EXTRA_FPS = "fps"
    const val EXTRA_MAX = "max"
    const val ACTION_STOP = "com.safedrop.app.STOP_SCREEN"
    private const val CHANNEL = "safedrop_screen"
    private const val NOTIFICATION_ID = 4711

    @JvmStatic external fun nativeFrame(jpeg: ByteArray, width: Int, height: Int)
    @JvmStatic external fun nativeStopped()
  }

  private var projection: MediaProjection? = null
  private var display: VirtualDisplay? = null
  private var reader: ImageReader? = null
  private var thread: HandlerThread? = null
  private var lastFrame = 0L
  private var frameGap = 83L
  private var stopped = false

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent == null || intent.action == ACTION_STOP) {
      stopSelf()
      return START_NOT_STICKY
    }
    startForegroundNotification()
    val code = intent.getIntExtra(EXTRA_CODE, 0)
    val data: Intent? = if (Build.VERSION.SDK_INT >= 33) {
      intent.getParcelableExtra(EXTRA_DATA, Intent::class.java)
    } else {
      @Suppress("DEPRECATION") intent.getParcelableExtra(EXTRA_DATA)
    }
    val fps = intent.getIntExtra(EXTRA_FPS, 12).coerceIn(1, 30)
    val maxSide = intent.getIntExtra(EXTRA_MAX, 1280).coerceIn(320, 1920)
    frameGap = 1000L / fps
    if (data == null) {
      stopSelf()
      return START_NOT_STICKY
    }
    try {
      start(code, data, maxSide)
    } catch (e: Exception) {
      stopSelf()
    }
    return START_NOT_STICKY
  }

  private fun start(code: Int, data: Intent, maxSide: Int) {
    val mpm = getSystemService(MediaProjectionManager::class.java)
    val proj = mpm.getMediaProjection(code, data) ?: throw IllegalStateException("no projection")
    projection = proj
    val t = HandlerThread("SafeDropScreen").also { it.start() }
    thread = t
    val handler = Handler(t.looper)
    proj.registerCallback(object : MediaProjection.Callback() {
      override fun onStop() { stopSelf() }
    }, handler)

    val dm = resources.displayMetrics
    val scale = minOf(1.0, maxSide.toDouble() / maxOf(dm.widthPixels, dm.heightPixels))
    val w = ((dm.widthPixels * scale).toInt() / 2) * 2
    val h = ((dm.heightPixels * scale).toInt() / 2) * 2
    val r = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 2)
    reader = r
    r.setOnImageAvailableListener({ rd ->
      val img = try { rd.acquireLatestImage() } catch (e: Exception) { null } ?: return@setOnImageAvailableListener
      try {
        val now = SystemClock.elapsedRealtime()
        if (now - lastFrame < frameGap) return@setOnImageAvailableListener
        lastFrame = now
        val plane = img.planes[0]
        val rowPixels = plane.rowStride / plane.pixelStride
        val bmp = Bitmap.createBitmap(rowPixels, h, Bitmap.Config.ARGB_8888)
        bmp.copyPixelsFromBuffer(plane.buffer)
        val frame = if (rowPixels != w) Bitmap.createBitmap(bmp, 0, 0, w, h) else bmp
        val out = ByteArrayOutputStream(64 * 1024)
        frame.compress(Bitmap.CompressFormat.JPEG, 60, out)
        if (frame !== bmp) frame.recycle()
        bmp.recycle()
        nativeFrame(out.toByteArray(), w, h)
      } catch (e: Exception) {
        // кадр пропущен — следующий придёт
      } finally {
        img.close()
      }
    }, handler)
    display = proj.createVirtualDisplay(
      "SafeDrop", w, h, dm.densityDpi,
      DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, r.surface, null, handler
    )
  }

  private fun startForegroundNotification() {
    val nm = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26 && nm.getNotificationChannel(CHANNEL) == null) {
      nm.createNotificationChannel(NotificationChannel(CHANNEL, "Демонстрация экрана", NotificationManager.IMPORTANCE_LOW))
    }
    val stop = android.app.PendingIntent.getService(
      this, 0, Intent(this, ScreenCaptureService::class.java).setAction(ACTION_STOP),
      android.app.PendingIntent.FLAG_IMMUTABLE
    )
    val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(this)
    val n = builder
      .setContentTitle("SafeDrop показывает экран")
      .setContentText("Нажми «Остановить», чтобы закончить демонстрацию")
      .setSmallIcon(R.drawable.ic_notification)
      .setOngoing(true)
      .addAction(Notification.Action.Builder(null, "Остановить", stop).build())
      .build()
    if (Build.VERSION.SDK_INT >= 29) {
      startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
    } else {
      startForeground(NOTIFICATION_ID, n)
    }
  }

  override fun onDestroy() {
    if (!stopped) {
      stopped = true
      try { display?.release() } catch (e: Exception) {}
      try { reader?.close() } catch (e: Exception) {}
      try { projection?.stop() } catch (e: Exception) {}
      thread?.quitSafely()
      try { nativeStopped() } catch (e: Throwable) {}
    }
    super.onDestroy()
  }
}
