package com.safedrop.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.media.projection.MediaProjectionManager
import android.webkit.MimeTypeMap
import androidx.activity.result.ActivityResult
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
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

@InvokeArg
class ScreenArgs {
  var fps: Int = 12
  var max: Int = 1280
}

// Открыть ссылку в браузере и полученный файл приложением телефона; начать и
// остановить демонстрацию экрана (ScreenCaptureService). Вызывается из Rust
// (src-tauri/src/mobile.rs), путь к файлу там уже проверен.
@TauriPlugin
class SdAndroidPlugin(private val activity: Activity) : Plugin(activity) {
  private var screenFps = 12
  private var screenMax = 1280

  @Command
  fun startScreenCapture(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(ScreenArgs::class.java)
      screenFps = args.fps
      screenMax = args.max
      val mpm = activity.getSystemService(MediaProjectionManager::class.java)
      // Android сам спрашивает, можно ли показывать экран
      startActivityForResult(invoke, mpm.createScreenCaptureIntent(), "onScreenCaptureResult")
    } catch (e: Exception) {
      invoke.reject(e.message ?: "error")
    }
  }

  @ActivityCallback
  fun onScreenCaptureResult(invoke: Invoke, result: ActivityResult) {
    val data = result.data
    if (result.resultCode != Activity.RESULT_OK || data == null) {
      invoke.reject("Показ экрана не разрешён")
      return
    }
    try {
      val i = Intent(activity, ScreenCaptureService::class.java)
        .putExtra(ScreenCaptureService.EXTRA_CODE, result.resultCode)
        .putExtra(ScreenCaptureService.EXTRA_DATA, data)
        .putExtra(ScreenCaptureService.EXTRA_FPS, screenFps)
        .putExtra(ScreenCaptureService.EXTRA_MAX, screenMax)
      ContextCompat.startForegroundService(activity, i)
      invoke.resolve()
    } catch (e: Exception) {
      invoke.reject(e.message ?: "error")
    }
  }

  @Command
  fun stopScreenCapture(invoke: Invoke) {
    activity.stopService(Intent(activity, ScreenCaptureService::class.java))
    invoke.resolve()
  }

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
