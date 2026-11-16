package com.safedrop.app

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

  /** Отступы от системных панелей для страницы (CSS-пиксели), JSON. */
  object Insets {
    @Volatile var json: String = "{\"top\":0,\"bottom\":0,\"left\":0,\"right\":0}"

    @JavascriptInterface
    fun get(): String = json
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
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    web = webView
    // страница читает отступы при загрузке (и после перезагрузки) — только числа
    webView.addJavascriptInterface(Insets, "SdInsets")
    findViewById<View>(android.R.id.content)?.let { ViewCompat.requestApplyInsets(it) }
  }
}
