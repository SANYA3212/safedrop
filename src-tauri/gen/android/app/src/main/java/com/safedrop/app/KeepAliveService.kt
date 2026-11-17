package com.safedrop.app

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
import androidx.core.content.ContextCompat

// Фоновая работа: пока служба с уведомлением запущена, Android не завершает
// процесс SafeDrop, когда окно закрыто или смахнуто из недавних, — связь с
// сервером остаётся, и звонки и сообщения приходят (CallNotifier). Окно при
// следующем открытии создаётся заново (MainActivity.nativeReopen).
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

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_QUIT) {
      // «Выключить»: служба и весь SafeDrop — до следующего запуска
      stopForeground(STOP_FOREGROUND_REMOVE)
      stopSelf()
      try { MainActivity.nativeQuit() } catch (e: Throwable) {}
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
      .setContentText("Звонки и сообщения приходят, даже когда приложение закрыто")
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
    return START_STICKY
  }

  // смахнули из недавних: служба остаётся (stopWithTask=false в манифесте)
  override fun onTaskRemoved(rootIntent: Intent?) {
    super.onTaskRemoved(rootIntent)
  }
}
