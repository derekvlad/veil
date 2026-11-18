# veil

Видеообои для Wayland, которые не съедают процессор.

`veil` — демон (`veild`) и клиент управления (`veilctl`). Демон рисует видео прямо
в слой рабочего стола через EGL, декодируя его железом, и останавливает
воспроизведение, когда обои всё равно не видно.

## Зачем

Типичный `mpvpaper` запускает mpv с `hwdec=no`, то есть декодирует 4K60 в софте.
Замеры на этой машине (Ryzen 7 5700X + Radeon RX 580, два монитора 1080p,
одно и то же видео 3840×2160, 60 fps, 62 Мбит/с):

| | CPU | RSS |
|---|---|---|
| `mpvpaper` (hwdec=no) | **≈ 80 %** одного ядра | 690 МБ |
| `veild` (VAAPI, то же 4K-видео) | **≈ 9 %** | 356 МБ |
| `veild` со статичной картинкой | **0 %** | — |

Аппаратное декодирование того же клипа: 18.3 CPU-секунды против 0.2 — то есть
драйвер умеет, ему просто не давали.

## Как это работает

```
композитор ── wl_surface (layer-shell, слой background)
                    │
                    ├── EGL + wl_egl_window  (нет копий через shm)
                    │        │
                    │        └── libmpv render API ── VAAPI/VDPAU/NVDEC
                    │                                    │
                    └──────────── dmabuf ────────────────┘
```

* **libmpv render API** — mpv не открывает своё окно и не трогает композитор;
  он рисует кадр в наш OpenGL FBO. Мы отвечаем только за поверхность и время.
* **`hwdec=auto-safe`** — декодирование на GPU, кадр приходит в EGL как dmabuf,
  без чтения в системную память.
* **Один плеер на монитор**, каждый со своим EGL-контекстом: на разных частотах
  обновления кадры не мешают друг другу.
* **Правила паузы** — фуллскрин (`zwlr-foreign-toplevel-management`), простой
  (`ext-idle-notify`), работа от батареи, ручная пауза. В паузе декодер спит,
  а не крутит кадры «в никуда».
* **Ноль лишнего**: без звука, OSD, привязок клавиш, скриншотов и блокировки
  ввода — клики по обоям уходят дальше в композитор.

Перемотка, кодеки, цветокоррекция, HDR-тонмаппинг — всё это уже внутри mpv,
поэтому собственный велосипед не нужен.

## Совместимость

| Окружение | Статус | Как |
|---|---|---|
| niri | ✅ работает, тестировалось | `zwlr-layer-shell` |
| Hyprland | ✅ должно работать | `zwlr-layer-shell` |
| sway / другие wlroots | ✅ должно работать | `zwlr-layer-shell` |
| KDE Plasma 6 | ⚠️ плагин в `plugins/kde` (не тестировался в этой сессии) | KPackage-плагин обоев |
| GNOME | ❌ пока нет | нужен shell-extension, см. `plugins/README.md` |

Почему так: `wlr-layer-shell` — это протокол, по которому композитор отдаёт
клиенту «фон». niri, Hyprland и sway его реализуют. KWin и Mutter фон внешним
клиентам не отдают в принципе, поэтому там нужен плагин внутри оболочки.

## Зависимости

* Rust ≥ 1.80 и `pkg-config`
* `libmpv` (в Arch — пакет `mpv`)
* Wayland, EGL, Mesa

```sh
# Arch
sudo pacman -S rust mpv
```

## Сборка и установка

```sh
./install.sh
```

Ставит `veild` и `veilctl` в `~/.local/bin`, unit systemd и — если найден
Plasma — плагин обоев в `~/.local/share/plasma/wallpapers/`.

## Использование

```sh
veilctl set ~/Videos/wallpaper.mp4     # поставить видео
veilctl set ~/Pictures/photo.png       # или картинку
veilctl set clip.mp4 --output DP-1     # только на один монитор
veilctl status                         # мониторы, декодер, кадры, пауза
veilctl pause | resume | toggle
veilctl reload                         # перечитать конфиг
veilctl quit
```

`status` показывает главное для диагностики — какой декодер реально выбран:

```json
"outputs": [{ "name": "DP-1", "pixels": {"width":1920,"height":1080},
              "codec": "H.264 / AVC ...", "decoder": "vaapi", "frames": 2411 }]
```

Если `decoder` равен `no`, железо не подхватилось: проверьте `vainfo`
и поставьте в конфиге `hwdec = "vaapi"` (или `vaapi-copy` для проблемных драйверов).

## Конфигурация

`~/.config/veil/config.toml` создаётся при первом запуске:

```toml
[wallpaper]
path = "/home/user/Videos/wallpaper.mp4"
mode = "fill"          # fill | fit | stretch | center
color = "#000000"

[playback]
hwdec = "auto-safe"    # auto-safe | vaapi | vaapi-copy | vulkan | no
scale_filter = "lanczos"
fps_limit = 0          # 0 = как в видео; 30 экономит upload/композитинг
vsync = false          # true — ровнее на одном мониторе
flip_y = false         # true, если картинка перевёрнута
mute = true

[rules]
pause_on_fullscreen = true
pause_on_maximized = false
pause_on_idle = true
idle_timeout_secs = 120
pause_on_battery = false

[outputs]
"HDMI-A-1" = "/home/user/Videos/second.mp4"
```

Изменения применяются по `veilctl reload` или `kill -HUP $(pidof veild)`.
Монитор можно указать и в `veilctl set ... --output`.

## Автозапуск

**niri** (`~/.config/niri/config.kdl`):

```kdl
spawn-at-startup "veild"
```

**systemd** (если в user-сессии проброшены `WAYLAND_DISPLAY` и `XDG_RUNTIME_DIR`,
как это делает uwsm):

```sh
systemctl --user daemon-reload
systemctl --user enable --now veil.service
```

Не запускайте одновременно другой видео-демон: оба слоя будут декодировать
видео, даже если виден только верхний. В Noctalia выберите статичные обои,
иначе она продолжит держать свой `mpvpaper`.

## Если что-то не так

* **Чёрный экран** — `veilctl status`: есть ли `outputs`, `configured: true`.
  Демон пишет в stderr, запустите `veild -v` из терминала.
* **Картинка перевёрнута** — `flip_y = true` в конфиге (зависит от драйвера).
* **Дёргается на двух мониторах** — оставьте `vsync = false`.
* **Высокий CPU** — посмотрите `decoder` в `status`. Если `no`, включите
  VAAPI явно и проверьте `vainfo`. Если `vaapi`, но всё равно много — уменьшите
  разрешение исходника: 4K на 1080p-экране декодируется вчетверо дороже нужного.

## Дорожная карта

* дробное масштабирование (`wp_fractional_scale` + viewporter)
* расширение GNOME Shell
* плейлисты и смена обоев по расписанию
* тонкая настройка пауз (по приложению, по видеозвонкам)

## Лицензия

MIT OR Apache-2.0.
