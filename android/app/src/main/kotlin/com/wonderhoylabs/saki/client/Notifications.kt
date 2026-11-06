package com.wonderhoylabs.saki.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import com.wonderhoylabs.saki.R
import com.wonderhoylabs.saki.ui.MainActivity

private const val CHANNEL_ID = "status"

/** Builds the notification a foreground service has to show. */
fun statusNotification(context: Context, text: String, actionLabel: String, actionIntent: Intent): Notification {
    val manager = context.getSystemService(NotificationManager::class.java)
    manager.createNotificationChannel(
        NotificationChannel(CHANNEL_ID, context.getString(R.string.notification_channel), NotificationManager.IMPORTANCE_LOW),
    )
    val open = PendingIntent.getActivity(
        context,
        0,
        Intent(context, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE,
    )
    val action = PendingIntent.getService(
        context,
        1,
        actionIntent,
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    return Notification.Builder(context, CHANNEL_ID)
        .setSmallIcon(R.drawable.ic_notification)
        .setContentTitle(context.getString(R.string.app_name))
        .setContentText(text)
        .setContentIntent(open)
        .setOngoing(true)
        .addAction(Notification.Action.Builder(null, actionLabel, action).build())
        .build()
}
