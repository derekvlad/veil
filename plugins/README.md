# Плагины для оболочек без layer-shell

`veil-gui` — не плагин: это обычное GTK4-приложение, которое настраивает демон
`veild` через его IPC. В niri, Hyprland и sway оно управляет обоями напрямую.
В Plasma обоями управляет плагин ниже, а `veild` и `veil-gui` можно не запускать.

## KDE Plasma 6 — `kde/org.veil.video`

Полноценный KPackage-плагин обоев: `MediaPlayer` + `VideoOutput` из Qt Multimedia
внутри `WallpaperItem`. Plasma сама прячет обои при фуллскрине, и мы на этом
основании ставим видео на паузу.

Установка:

```sh
install -d ~/.local/share/plasma/wallpapers
cp -r plugins/kde/org.veil.video ~/.local/share/plasma/wallpapers/
# перезапустить plasmashell или перелогиниться
```

Затем: ПКМ по рабочему столу → «Настроить рабочий стол» → «Обои» → **Veil Video**.

Ограничения:

* аппаратное декодирование зависит от бэкенда Qt Multimedia (FFmpeg/GStreamer);
  `veilctl status` тут не при чём — это отдельный путь;
* нет общих правил паузы `veil` (простой, батарея) — только скрытие обоев;
* демон `veild` для Plasma не нужен и запускать его не стоит: он рассчитан на
  layer-shell.

## GNOME — пока нет

Mutter не отдаёт фон внешним клиентам и не реализует `wlr-layer-shell`, поэтому
вариант «клиент рисует в фон» здесь невозможен. Рабочий путь — расширение
GNOME Shell, которое вставляет актёр под слой окон и подаёт в него кадры
GStreamer (`playbin` + sink, отдающий `Clutter.Content`), плюс пауза по
`global.display` сигналам о фуллскрине и по `ScreenSaver` для блокировки.

Это отдельный проект: нужен GJS/C, жизненный цикл расширения и совместимость
с GNOME 45+. Поэтому в v0.1 GNOME не заявлен, чтобы не отдавать
неработающую заглушку.
