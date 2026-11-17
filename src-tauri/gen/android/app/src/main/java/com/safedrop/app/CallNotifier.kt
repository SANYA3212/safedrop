package com.safedrop.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build

// Входящий звонок, когда окна SafeDrop не видно (свёрнуто, закрыто, экран
// заблокирован): уведомление со звонком на весь экран, как в Telegram, —
// «Ответить» открывает SafeDrop и принимает звонок, «Отклонить» сбрасывает.
// Вызывает Rust (mobile.rs) через JNI; сам звонок принимает страница.
object CallNotifier {
  const val CHANNEL = "safedrop_calls_v1"
  const val MISSED_CHANNEL = "safedrop_missed"
  const val EXTRA_IP = "sd_call_ip"
  const val EXTRA_ANSWER = "sd_call_answer"
  const val ACTION_DECLINE = "com.safedrop.app.DECLINE"
  private const val NOTIFICATION_ID = 4713
  private const val MISSED_ID = 4714

  private fun channels(ctx: Context) {
    if (Build.VERSION.SDK_INT < 26) return
    val nm = ctx.getSystemService(NotificationManager::class.java)
    if (nm.getNotificationChannel(CHANNEL) == null) {
      val ch = NotificationChannel(CHANNEL, "Входящие звонки", NotificationManager.IMPORTANCE_HIGH)
      ch.description = "Звонок, когда SafeDrop закрыт или экран заблокирован"
      ch.setSound(
        RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
        AudioAttributes.Builder()
          .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
          .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
          .build()
      )
      ch.enableVibration(true)
      ch.vibrationPattern = longArrayOf(0, 800, 600, 800, 600, 800)
      ch.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
      nm.createNotificationChannel(ch)
    }
    if (nm.getNotificationChannel(MISSED_CHANNEL) == null) {
      nm.createNotificationChannel(NotificationChannel(MISSED_CHANNEL, "Пропущенные звонки", NotificationManager.IMPORTANCE_DEFAULT))
    }
  }

  @JvmStatic
  fun show(ctx: Context, ip: String, title: String, text: String) {
    // окно на экране — звонит сама страница
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
        .setSound(RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE))
    }
    val n = b.build()
    n.flags = n.flags or Notification.FLAG_INSISTENT // звонит, пока не ответят
    try {
      ctx.getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, n)
    } catch (e: SecurityException) {
      // нет разрешения на уведомления
    }
  }

  @JvmStatic
  fun cancel(ctx: Context) {
    ctx.getSystemService(NotificationManager::class.java).cancel(NOTIFICATION_ID)
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

/// «Отклонить» в уведомлении звонка.
class CallActionReceiver : BroadcastReceiver() {
  override fun onReceive(ctx: Context, intent: Intent) {
    if (intent.action != CallNotifier.ACTION_DECLINE) return
    CallNotifier.cancel(ctx)
    val ip = intent.getStringExtra(CallNotifier.EXTRA_IP) ?: return
    try { MainActivity.nativeDecline(ip) } catch (e: Throwable) {}
  }
}
