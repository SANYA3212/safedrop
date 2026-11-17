package com.safedrop.app

import android.app.KeyguardManager
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  private var web: WebView? = null

  companion object {
    /** Окно SafeDrop сейчас на экране (тогда звонит сама страница). */
    @JvmStatic @Volatile var onScreen = false
    /** Сколько окон живо; было ли окно в этом процессе (см. onCreate). */
    private var alive = 0
    private var hadWindow = false

    // реализованы в Rust (mobile.rs)
    @JvmStatic external fun nativeReopen()
    @JvmStatic external fun nativeAnswer(ip: String)
    @JvmStatic external fun nativeDecline(ip: String)
    @JvmStatic external fun nativeQuit()
  }

  /** Отступы от системных панелей для страницы (CSS-пиксели), JSON. */
  object Insets {
    @Volatile var json: String = "{\"top\":0,\"bottom\":0,\"left\":0,\"right\":0}"

    @JavascriptInterface
    fun get(): String = json

    /** Окно на экране: тогда звонок звонит страница, иначе — уведомление. */
    @JavascriptInterface
    fun shown(): Boolean = onScreen
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    // Окно — на весь экран, строка состояния и кнопки навигации прозрачные
    // (светлые значки: интерфейс тёмный). Без своих полос: под ними виден
    // фон приложения, а шапка и поле ввода сами отступают (mobile.css, --sat/--sab).
    enableEdgeToEdge(
      SystemBarStyle.dark(Color.TRANSPARENT),
      SystemBarStyle.dark(Color.TRANSPARENT)
    )
    super.onCreate(savedInstanceState)
    if (Build.VERSION.SDK_INT >= 29) {
      window.isNavigationBarContrastEnforced = false
      window.isStatusBarContrastEnforced = false
    }
    val root = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
      val keyboard = insets.isVisible(WindowInsetsCompat.Type.ime()) && ime.bottom > bars.bottom
      // клавиатура: страница кончается над ней (поле ввода над клавиатурой);
      // иначе страница до самого низа, а отступ от кнопок делает она сама
      v.setPadding(0, 0, 0, if (keyboard) ime.bottom else 0)
      val d = resources.displayMetrics.density
      val css = { px: Int -> Math.round(px / d) }
      Insets.json = "{\"top\":${css(bars.top)},\"bottom\":${if (keyboard) 0 else css(bars.bottom)},\"left\":${css(bars.left)},\"right\":${css(bars.right)}}"
      web?.evaluateJavascript("window.__sdInsets&&window.__sdInsets(${Insets.json})", null)
      WindowInsetsCompat.CONSUMED
    }
    // Окно закрывали, а SafeDrop остался работать в фоне (KeepAliveService):
    // ядро живо, страницы нет — создать окно заново.
    if (alive == 0 && hadWindow) {
      try { nativeReopen() } catch (e: Throwable) {}
    }
    alive++
    hadWindow = true
    handleCallIntent(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    handleCallIntent(intent)
  }

  /** Открыли из уведомления о звонке: поверх блокировки, «Ответить» — принять. */
  private fun handleCallIntent(intent: Intent?) {
    val ip = intent?.getStringExtra(CallNotifier.EXTRA_IP) ?: return
    val answer = intent.getBooleanExtra(CallNotifier.EXTRA_ANSWER, false)
    intent.removeExtra(CallNotifier.EXTRA_IP)
    intent.removeExtra(CallNotifier.EXTRA_ANSWER)
    CallNotifier.cancel(this)
    if (Build.VERSION.SDK_INT >= 27) {
      setShowWhenLocked(true)
      setTurnScreenOn(true)
    }
    if (answer) {
      // принять звонок; дальше — снять блокировку (отпечаток / код)
      try { nativeAnswer(ip) } catch (e: Throwable) {}
      if (Build.VERSION.SDK_INT >= 26) {
        getSystemService(KeyguardManager::class.java)?.requestDismissKeyguard(this, null)
      }
    }
  }

  override fun onResume() {
    super.onResume()
    onScreen = true
  }

  override fun onPause() {
    onScreen = false
    super.onPause()
  }

  override fun onStop() {
    super.onStop()
    // поверх блокировки — только пока идёт звонок с экрана блокировки
    if (Build.VERSION.SDK_INT >= 27) setShowWhenLocked(false)
  }

  override fun onDestroy() {
    if (!isChangingConfigurations) alive = maxOf(0, alive - 1)
    web = null
    super.onDestroy()
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    web = webView
    // страница читает отступы при загрузке (и после перезагрузки) — только числа
    webView.addJavascriptInterface(Insets, "SdInsets")
    findViewById<View>(android.R.id.content)?.let { ViewCompat.requestApplyInsets(it) }
  }
}
