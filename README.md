# veil

Видеообои для Wayland, которые не съедают процессор.

`veil` — это демон `veild`, клиент `veilctl` и графический выбор обоев `veil-gui`.
Демон рисует видео прямо в слой рабочего стола через EGL, декодируя его железом,
и останавливает воспроизведение, когда обои всё равно не видно.

![Выбор обоев в veil-gui](docs/screenshot.png)

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
* **Один демон на сессию** — `flock` на `$XDG_RUNTIME_DIR/veil.lock`: если
  автозапуск сработал дважды (niri + systemd), второй процесс тихо выходит.
* **Ноль лишнего**: без звука, OSD, привязок клавиш, скриншотов и блокировки
  ввода — клики по обоям уходят дальше в композитор.

Перемотка, кодеки, цветокоррекция, HDR-тонмаппинг — всё это уже внутри mpv,
поэтому собственный велосипед не нужен.

## Выбор обоев (GUI)

`veil-gui` — обычное GTK4/libadwaita-приложение, поэтому выглядит родным и в
niri, и в KDE, и в GNOME:

* библиотека обоев сеткой с постерами, разрешением и длительностью;
* клик по обоям применяет их на выбранный экран («Все мониторы» или конкретный);
* панель мониторов: разрешение, текущий файл и отметка «свои» для экранов с
  собственными обоями, сброс в один клик;
* режимы `fill`, `fit`, `stretch`, `center` — применяются сразу;
* «Один кадр на все экраны» — одна картинка растягивается на весь рабочий стол,
  каждый монитор показывает свою часть без искажений;
* плейлист: автосмена с интервалом, случайный порядок, плавное появление,
  закрепление файлов правым кликом (пустой список = вся библиотека);
* перетаскивание видео и картинок прямо в окно — файлы копируются в библиотеку;
* папка библиотеки отслеживается: новые файлы появляются в сетке сами;
* «Добавить» копирует видео или картинку в библиотеку
  (`~/.local/share/veil/wallpapers`), правый клик — «Удалить из библиотеки»
  (файл уходит в корзину);
* переключатели «без звука», пауза в фуллскрине/при простое/от батареи и
  таймаут простоя;
* случайные обои, поиск по библиотеке, статус демона и кнопка его запуска;
* всё, что меняется в окне, сразу пишется в `~/.config/veil/config.toml`.

Интерфейс на русском или английском — по `LANG`. Постеры для сетки делает
`ffmpeg` и кэширует в `~/.cache/veil/thumbs`; без ffmpeg GUI работает, но
показывает иконки вместо кадров.

## Плейлист и один кадр на все экраны

Плейлист живёт в конфиге и не зависит от GUI. По умолчанию он берёт все файлы
из библиотеки; `[playlist.outputs]` задаёт отдельные списки для конкретных
мониторов. Мониторы без своего списка меняют обои синхронно, со своим — живут
отдельно.

```toml
[playlist]
enabled = true
interval_secs = 300     # минимум 5
shuffle = true
fade_secs = 1.2         # появление из чёрного, 0 — выключить
items = []              # пусто = вся библиотека

[playlist.outputs]
"DP-1" = ["/home/user/Videos/left.mp4"]
```

Смена не переписывает конфиг: `wallpaper.path` остаётся тем, что выбрал
пользователь, а текущий файл идёт в `veilctl status` и в GUI.

Span (`wallpaper.span = true`) включает режим «один кадр на все экраны». Каждый
монитор по-прежнему декодирует своё, но mpv показывает нужный кусок общей
картинки: вычисляется общий bounding box мониторов, видео масштабируется под
него, а `video-zoom`/`video-pan-x`/`video-pan-y` сдвигают кадр на каждом экране.
Так 21:9-клип покрывает два 16:9-монитора без полос и растяжения. Индивидуальные
обои при этом игнорируются; выбор обоев для одного монитора выключает span.

## D-Bus

Демон поднимает `org.veil.Veild` на `/org/veil/Veil` (отдельное имя, потому что
`org.veil.Veil` занято самим GUI как GApplication id):

```sh
busctl --user call org.veil.Veild /org/veil/Veil org.veil.Veil Next
busctl --user call org.veil.Veild /org/veil/Veil org.veil.Veil Random
busctl --user call org.veil.Veild /org/veil/Veil org.veil.Veil Toggle
busctl --user get-property org.veil.Veild /org/veil/Veil org.veil.Veil PlaybackStatus
```

Методы: `Pause`, `Resume`, `Toggle`, `Next`, `Random`, `Status` (JSON).
Свойства: `PlaybackStatus` (`Playing`/`Paused`), `Version`. Если сессионной шины
нет, демон просто пишет предупреждение и работает дальше.

## Виджет Noctalia

В баре Noctalia достаточно CustomButton: левый клик — пауза, средний — следующее
обои, правый — окно выбора. В `~/.local/state/noctalia/settings.toml`:

```toml
[widget.veil]
type = "custom_button"
glyph = "movie"

[widget.veil.actions]
left = "exec veilctl toggle"
middle = "exec veilctl next"
right = "exec veil-gui"
```

После правки — `noctalia config validate && noctalia msg config-reload`.


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
`veil-gui` при этом остаётся просто настройкой демона: на Plasma обоями
управляет плагин, а не `veild`.

## Зависимости

* Rust ≥ 1.85 и `pkg-config`
* `libmpv` (в Arch — пакет `mpv`)
* Wayland, EGL, Mesa
* GTK4 + libadwaita (для `veil-gui`)
* `ffmpeg`/`ffprobe` — для постеров и метаданных (необязательно)

```sh
# Arch
sudo pacman -S rust mpv gtk4 libadwaita ffmpeg
```

## Сборка и установка

```sh
./install.sh
```

Скрипт собирает весь workspace, ставит `veild`, `veilctl` и `veil-gui` в
`~/.local/bin`, desktop-файл и иконку в `~/.local/share`, unit systemd и — если
найден Plasma — плагин обоев.

Структура репозитория:

```
crates/core     библиотека veil: EGL, mpv, layer-shell, конфиг, IPC, span, D-Bus-клиент
crates/daemon   veild — фоновый демон и сервис org.veil.Veild
crates/ctl      veilctl — управление из терминала и скриптов
crates/gui      veil-gui — GTK4-выбор обоев
packaging       desktop-файл, иконка, systemd unit, PKGBUILD
plugins         KDE-плагин и заметки про GNOME
.github         CI: fmt, тесты и release-сборка в контейнере Arch
```

Проверки:

```sh
cargo fmt --all -- --check
cargo test --workspace
```

Тесты покрывают разбор и round-trip конфига, serde-формат IPC, выбор файлов
плейлиста и математику span (bounding box, zoom/pan для двух экранов,
панорамный исходник, вырожденные значения). Для AUR есть
`packaging/PKGBUILD` (пакет `veil-git`).

## Использование

```sh
veil-gui                                # выбрать обои мышкой
veilctl set ~/Videos/wallpaper.mp4      # поставить видео
veilctl set ~/Pictures/photo.png        # или картинку
veilctl set clip.mp4 --output DP-1      # только на один монитор
veilctl next [--output DP-1]            # следующее обои плейлиста
veilctl random [--output DP-1]          # случайное из плейлиста
veilctl clear [--output DP-1]           # снять индивидуальные обои
veilctl span on|off|toggle              # один кадр на все экраны
veilctl playlist on|off|toggle          # автосмена
veilctl status                          # мониторы, декодер, кадры, пауза
veilctl pause | resume | toggle
veilctl reload                          # перечитать конфиг
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
path = "/home/user/.local/share/veil/wallpapers/wallpaper.mp4"
mode = "fill"          # fill | fit | stretch | center
color = "#000000"
span = false           # один кадр на все мониторы

[playback]
hwdec = "auto-safe"    # auto-safe | vaapi | vaapi-copy | vulkan | no
scale_filter = "lanczos"
fps_limit = 0          # 0 = как в видео; 30 экономит upload/композитинг
vsync = false          # true — ровнее на одном мониторе
flip_y = true          # GL-путь пишет кадр снизу вверх; false — если перевёрнуто
mute = true

[rules]
pause_on_fullscreen = true
pause_on_maximized = false
pause_on_idle = true
idle_timeout_secs = 120
pause_on_battery = false

[library]
path = "~/.local/share/veil/wallpapers"

[playlist]
enabled = false
interval_secs = 300
shuffle = true
fade_secs = 1.2
items = []

[playlist.outputs]
# "DP-1" = ["/home/user/Videos/left.mp4"]

[outputs]
"HDMI-A-1" = "/home/user/Videos/second.mp4"
```

`veilctl set` и `veilctl clear` сами сохраняют конфиг, поэтому их результат
переживает перезапуск демона. Автосмена, наоборот, ничего не пишет на диск.

Изменения применяются по `veilctl reload` или `kill -HUP $(pidof veild)`.
Монитор можно указать и в `veilctl set ... --output`. GUI пишет этот файл сам,
поэтому комментарии в нём не сохраняются — держите личные пометки вне конфига.

## Автозапуск

**niri** (`~/.config/niri/config.kdl`) — именно так настроено на этой машине:

```kdl
spawn-at-startup "/home/derek/.local/bin/veild"
```

Путь лучше писать абсолютным: на старте сессии `~/.local/bin` может ещё не быть
в `PATH`.

**systemd** (если в user-сессии проброшены `WAYLAND_DISPLAY` и `XDG_RUNTIME_DIR`,
как это делает uwsm):

```sh
systemctl --user daemon-reload
systemctl --user enable --now veil.service
```

Можно включить оба способа: `flock` не даст запуститься второй копии.

Не запускайте одновременно другой видео-демон: оба слоя будут декодировать
видео, даже если виден только верхний. В Noctalia обои выключены
(`wallpaper.enabled = false`), иначе она продолжит запускать свой `mpvpaper`.

## Если что-то не так

* **Чёрный экран** — `veilctl status`: есть ли `outputs`, `configured: true`.
  Демон пишет в stderr, запустите `veild -v` из терминала.
* **GUI не видит демон** — проверьте `$XDG_RUNTIME_DIR/veil.sock` и запустите
  `veild`; кнопка «Запустить демон» в окне делает то же самое.
* **Картинка перевёрнута** — поставьте `flip_y = false` (по умолчанию `true`,
  потому что GL-кадр приходит снизу вверх) и сделайте `veilctl reload`.
* **Дёргается на двух мониторах** — оставьте `vsync = false`.
* **Высокий CPU** — посмотрите `decoder` в `status`. Если `no`, включите
  VAAPI явно и проверьте `vainfo`. Если `vaapi`, но всё равно много — уменьшите
  разрешение исходника: 4K на 1080p-экране декодируется вчетверо дороже нужного.

## Дорожная карта

* дробное масштабирование (`wp_fractional_scale` + viewporter)
* расширение GNOME Shell
* тонкая настройка пауз (по приложению, по видеозвонкам)
* полноценный кроссфейд между двумя mpv-плеерами

## Лицензия

MIT.
