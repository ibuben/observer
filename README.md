# eXUnity Observer

Программа видеонаблюдения для регистраторов **Xiongmai (XM)** и **Hikvision** по локальному или белому IP: живое видео до 64 камер, архив, скачивание записей в MP4, звук, PTZ, звонок домофона.

В проекте две программы:

- `xm-view` — графическое приложение eXUnity Observer;
- `xm-nvr` — консольная утилита: информация о регистраторе, поиск записей, запись живого видео и архива в файл, диагностика подключения Hikvision.

Работает на **Windows** и **Linux**.

## Что поддерживается

| | Xiongmai (XM) | Hikvision |
| --- | --- | --- |
| Живое видео, H.264 и H.265 | ✓ | ✓ камеры, регистраторы (NVR), домофоны |
| Звук с камер | ✓ | ✓ G.711, AAC, PCM |
| Архив: поиск, перемотка, скорость | ✓ | ✓ |
| Скачивание записей в MP4 | ✓ | ✓ |
| PTZ | ✓ | — |
| Журнал событий | ✓ | — |
| Конфигуратор | ✓ | — |
| Домофон: звонок и открытие замка | — | ✓ |

### Подключение Hikvision

- В программе указывается HTTP-порт устройства (обычно `80`): через него идут сведения об устройстве, список камер, поиск и скачивание записей (ISAPI). Видео и звук идут по RTSP на порт `554`. При подключении через белый IP пробросьте оба порта.
- На устройстве должен быть включён ISAPI: Конфигурация → Сеть → Расширенные настройки → Протокол интеграции.
- Чтобы был звук, выберите для потока камеры тип «Видео и аудио» и аудиокодек G.711 или AAC.

## Готовые сборки

Скачайте архив для своей системы на странице [Releases](https://github.com/ibuben/observer/releases/latest):

- `observer-vX.Y.Z-windows-x64.zip` — Windows 10/11, 64 бит;
- `observer-vX.Y.Z-linux-x64.tar.gz` — Linux x86-64 с glibc 2.31 или новее (Ubuntu 20.04+, Debian 11+, Fedora 32+ и т. п.).

Распакуйте архив в папку, доступную на запись, и запустите `xm-view` (`xm-view.exe`). Отдельно нужен только **ffmpeg** (см. ниже). На Linux он ставится из пакетов, например `sudo apt install ffmpeg`.

## Что нужно для работы

- **Rust** 1.88 или новее, ставится через [rustup](https://rustup.rs). Rust из пакетов дистрибутива (`apt install rustc`) обычно слишком старый.
- **ffmpeg** 4.2 или новее — им распаковывается видео и собираются MP4. Программа ищет `C:\ffmpeg\ffmpeg.exe` (только Windows), а если его нет — `ffmpeg` в `PATH`.

## Сборка на Windows

1. Установите [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) с компонентом «Разработка классических приложений на C++».
2. Установите Rust: скачайте и запустите [rustup-init.exe](https://rustup.rs), согласитесь с настройками по умолчанию.
3. Установите ffmpeg: скачайте сборку с [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) и положите `ffmpeg.exe` в `C:\ffmpeg\` или в любую папку из `PATH`.
4. Соберите проект:

   ```powershell
   git clone https://github.com/ibuben/observer.git
   cd observer
   cargo build --release
   ```

5. Готовые файлы лежат в `target\release\`: `xm-view.exe` и `xm-nvr.exe`.

## Сборка на Linux

### 1. Системные пакеты

Для сборки нужны компилятор C (как линковщик), `pkg-config` и заголовки ALSA для звука. Для работы — ffmpeg и обычные библиотеки графического окружения (X11 или Wayland, OpenGL), которые на десктопе обычно уже есть.

**Debian / Ubuntu / Mint:**

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libasound2-dev ffmpeg git curl \
    libxkbcommon-x11-0 libgl1 libegl1
```

**Fedora:** ffmpeg берётся из [RPM Fusion](https://rpmfusion.org/Configuration).

```bash
sudo dnf install -y gcc pkgconf-pkg-config alsa-lib-devel git curl \
    libxkbcommon-x11 mesa-libGL mesa-libEGL
sudo dnf install -y ffmpeg
```

**Arch / Manjaro:**

```bash
sudo pacman -S --needed base-devel pkgconf alsa-lib ffmpeg git libxkbcommon-x11 mesa
```

Если `apt` пишет `Unable to locate package libasound2-dev`, значит, в системе не подключён основной репозиторий Ubuntu (бывает после обновления версии или установки с диска). Проверьте: `grep -rh '^deb ' /etc/apt/sources.list /etc/apt/sources.list.d/` — среди строк должна быть `deb http://...ubuntu... <версия> main`. Для Ubuntu 20.04 его можно добавить так, затем повторить `apt update` и установку:

```bash
sudo tee /etc/apt/sources.list.d/ubuntu-main.list >/dev/null <<'EOF'
deb http://archive.ubuntu.com/ubuntu focal main restricted universe multiverse
deb http://archive.ubuntu.com/ubuntu focal-updates main restricted universe multiverse
EOF
```

### 2. Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

Если Rust уже стоит, обновите его: `rustup update`.

### 3. Сборка и запуск

```bash
git clone https://github.com/ibuben/observer.git
cd observer
cargo build --release
./target/release/xm-view
```

Первая сборка занимает несколько минут.

### Ярлык в меню приложений (по желанию)

```bash
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons
cp target/release/xm-view target/release/xm-nvr ~/.local/bin/
cp Observer.png ~/.local/share/icons/observer.png
cat > ~/.local/share/applications/observer.desktop <<EOF
[Desktop Entry]
Type=Application
Name=eXUnity Observer
Exec=$HOME/.local/bin/xm-view
Icon=$HOME/.local/share/icons/observer.png
Categories=Video;AudioVideo;
EOF
```

### Особенности Linux

- **Звук** идёт через ALSA. В системах с PulseAudio или PipeWire он попадает туда через стандартный ALSA-плагин, отдельно ничего настраивать не нужно.
- **Аппаратное ускорение** включается в настройках программы. На Linux ffmpeg сам выбирает VA-API (Intel/AMD), VDPAU или CUDA (NVIDIA). Если ускорение недоступно, например в виртуальной машине, видео распаковывается процессором.
- В виртуальной машине без 3D-ускорения окно рисуется программным OpenGL (Mesa llvmpipe). Это работает, но при большом числе камер будет медленнее.

## Где программа хранит данные

| Что | Где |
| --- | --- |
| Настройки и список регистраторов (`xm-settings.json`) | рядом с исполняемым файлом |
| Снимки кадров (`snapshots/`) | рядом с исполняемым файлом |
| Скачанные записи | `~/Downloads/Observer` (Windows: `%USERPROFILE%\Downloads\Observer`) |

Поэтому папка с `xm-view` должна быть доступна на запись. В `xm-settings.json` пароли от регистраторов хранятся открытым текстом — не выкладывайте этот файл в общий доступ.

## Консольная утилита `xm-nvr`

```bash
# Информация о регистраторе
xm-nvr info --host 192.168.1.10

# Записи канала 1 за сутки
xm-nvr search --host 192.168.1.10 --channel 1 \
    --start "2026-10-06 00:00:00" --end "2026-10-06 23:59:59"

# 30 секунд живого видео с канала 2 в файл
xm-nvr live --host 192.168.1.10 --channel 2 --stream main --seconds 30 --out cam2.h264
```

Порт по умолчанию — `34567`, пользователь — `admin`. Пароль передаётся через `--password` или переменную окружения `XM_PASSWORD`. Полный список параметров: `xm-nvr --help`.

Для Hikvision есть команда диагностики `hik` (порт по умолчанию `80`). Она показывает список камер, весь разговор по RTSP с ответами устройства и что реально приходит в потоке:

```bash
# Модель, прошивка и камеры с номерами каналов
xm-nvr hik --host 192.168.1.20

# 5 секунд живого видео камеры 1: кодек, звук, типы кадров; поток сохраняется в файл
xm-nvr hik --host 192.168.1.20 --channel 1 --seconds 5 --out cam1.h264

# Записи камеры 1 за час, скачивание первой и проверка её воспроизведения
xm-nvr hik --host 192.168.1.20 --channel 1 \
    --start "2026-10-06 10:00:00" --end "2026-10-06 11:00:00" --download first.ps
```
