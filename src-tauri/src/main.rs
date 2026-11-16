#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Всё приложение — в lib.rs: та же библиотека собирается и под Android.
fn main() {
    safedrop_lib::run()
}
