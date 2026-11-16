package com.safedrop.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class UrlArgs {
  lateinit var url: String
}

@InvokeArg
class FileArgs {
  lateinit var path: String
}

// Открыть ссылку в браузере и полученный файл приложением телефона.
// Вызывается из Rust (src-tauri/src/mobile.rs), путь там уже проверен.
@TauriPlugin
class SdAndroidPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun openUrl(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(UrlArgs::class.java)
      val uri = Uri.parse(args.url)
      if (uri.scheme != "https" && uri.scheme != "http") {
        invoke.reject("bad url")
        return
      }
      activity.startActivity(Intent(Intent.ACTION_VIEW, uri))
      invoke.resolve()
    } catch (e: ActivityNotFoundException) {
      invoke.reject("Нет браузера, чтобы открыть ссылку")
    } catch (e: Exception) {
      invoke.reject(e.message ?: "error")
    }
  }

  @Command
  fun openFile(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(FileArgs::class.java)
      val file = File(args.path)
      val uri = FileProvider.getUriForFile(activity, activity.packageName + ".fileprovider", file)
      val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*"
      val intent = Intent(Intent.ACTION_VIEW)
        .setDataAndType(uri, mime)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      activity.startActivity(intent)
      invoke.resolve()
    } catch (e: ActivityNotFoundException) {
      invoke.reject("На телефоне нет приложения, которое открывает такие файлы")
    } catch (e: Exception) {
      invoke.reject(e.message ?: "error")
    }
  }
}
