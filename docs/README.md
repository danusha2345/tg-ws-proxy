# TG WS Proxy — Rust

## Скачать с GitLab

Если GitHub недоступен, используйте публичное зеркало с файлами на GitLab:

- [Windows, Linux и macOS — Rust 1.11.0](https://gitlab.com/pipecpriam/tg-ws-proxy/-/releases/rust-v1.11.0)
- [Android 0.2.0 — подписанные APK](https://gitlab.com/pipecpriam/tg-ws-proxy/-/releases/android-v0.2.0)
- [Исходники и ветки](https://gitlab.com/pipecpriam/tg-ws-proxy)

Контрольные суммы находятся в файлах `SHA256SUMS.txt` и
`SHA256SUMS-android.txt` внутри соответствующего релиза. Встроенный updater
версий 1.11.0 / 0.2.0 пока обращается к GitHub; зеркало используется для
ручного скачивания.


[![Boosty](https://img.shields.io/badge/Boosty-Поддержать-FF7143?style=for-the-badge&logo=boosty&logoColor=white)](https://boosty.to/danusha/donate)

### Поддержать разработку форка

- [Boosty](https://boosty.to/danusha/donate)
- USDT (TRON / TRC20): `THyBqiMTWQ7kUH6vVBEdboL7yGLj5mCSrX`
- GRAM (TON): `UQDOgjGljFVJiHo_c9JLuX4hF2UQ2SXqSXhj3-1RefFMA4tB`

Локальный MTProto-прокси для Telegram Desktop. Он перенаправляет трафик через
TLS WebSocket и автоматически использует доступный резервный маршрут, не
требуя отдельного пользовательского сервера для базового режима.

> [!WARNING]
>
> Старый tray проверялся на Windows 11. Новое окно требует отдельной проверки на реальном Windows. Сборки macOS, Linux и Windows ARM64 проходят CI,
> но ещё нуждаются в расширенном пользовательском тестировании на реальном
> железе, включая direct WebSocket, Worker/CfProxy, TCP fallback и Fake TLS.
> [Сообщайте о найденных проблемах в Issues](https://github.com/danusha2345/tg-ws-proxy/issues).

## Что уже реализовано

- MTProto obfuscation init и AES-CTR relay в обоих направлениях;
- direct TLS WebSocket, domain fronting, Cloudflare Worker/CfProxy и TCP
  fallback;
- Fake TLS и HAProxy PROXY protocol v1;
- пулы direct и Cloudflare Worker соединений, лимиты клиентов и размера
  WebSocket-сообщений;
- постоянный secret, ротация логов и Docker-образ без root;
- окно управления и редактор настроек для Windows 10+, tray для macOS и Linux;
- встроенная загрузка стабильных обновлений с GitHub Releases с проверкой
  SHA-256.

Подробный статус, исправленные ошибки и известные ограничения описаны в
[документе о Rust-порте](./RUST_PORT.md).

## Готовые стабильные сборки

Собственные Rust-сборки публикуются на
[странице Releases](https://github.com/danusha2345/tg-ws-proxy/releases).

| Система | Артефакт |
| --- | --- |
| Windows 10+ x64 | `TgWsProxy_windows_x64.exe` |
| Windows 11 ARM64 | `TgWsProxy_windows_arm64.exe` |
| macOS 11+ Intel / Apple Silicon | `TgWsProxy_macos_universal.dmg` |
| Linux x86_64 | `TgWsProxy_linux_amd64`, `.deb` или `.rpm` |
| Linux ARM64 | `TgWsProxy_linux_arm64`, `.deb` или `.rpm` |

В архивах также есть отдельный CLI. Windows-бинарники пока не подписаны, а
macOS-приложение не notarized. Rust-сборки для Windows 7 не выпускаются:
актуальный Rust toolchain эту систему не поддерживает.

## Сборка из исходников

Нужен Rust `1.85` или новее:

```bash
git clone https://github.com/danusha2345/tg-ws-proxy.git
cd tg-ws-proxy
cargo build --release --locked
./target/release/tg-ws-proxy \
  --secret-file "$HOME/.local/state/tg-ws-proxy/secret"
```

После запуска прокси напечатает ссылку `tg://proxy`. Откройте её в Telegram
Desktop или добавьте MTProto-прокси вручную:

- сервер: `127.0.0.1`;
- порт: `1443`;
- secret: значение из напечатанной ссылки.

Secret сохраняется в указанном файле и повторно используется при следующих
запусках. Не публикуйте его и не добавляйте в Git.

### Окно Windows и системный трей

```bash
cargo run --release --locked \
  --features desktop \
  --bin tg-ws-proxy-desktop
```

Windows открывает окно управления с формой настроек; закрытие окна оставляет
прокси в трее. Linux и macOS сохраняют прежний tray. Используется тот же Rust runtime. Пункт обновления проверяет стабильные
`rust-v*` релизы, скачивает сборку для текущей ОС и сверяет её с
`SHA256SUMS.txt`. Windows autostart и portable mode пока остаются возможностями legacy
Python-версии. [Описание нового интерфейса](./RUST_PORT.md).

### Docker

```bash
docker build -t tg-ws-proxy:rust .
docker volume create tg-ws-proxy-data
docker run -d \
  --name tg-ws-proxy \
  --restart unless-stopped \
  -p 1443:1443 \
  -v tg-ws-proxy-data:/data \
  tg-ws-proxy:rust
```

Полная инструкция: [TG WS Proxy для Docker](./README.docker.md).

## Нужны тестеры

Особенно полезны проверки на Windows, macOS и разных окружениях Linux:

1. запуск CLI и tray;
2. подключение Telegram по напечатанной ссылке;
3. сообщения, фото, видео и большие файлы;
4. direct WebSocket, Worker/CfProxy и TCP fallback;
5. Fake TLS, sleep/resume и длительная работа.

В отчёте укажите ОС, способ запуска, проверенный маршрут и приложите логи без
secret. Баги и результаты тестов принимаются в
[Issues](https://github.com/danusha2345/tg-ws-proxy/issues).

## Как это работает

```text
Telegram Desktop → MTProto Proxy (127.0.0.1:1443) → TLS WebSocket → Telegram DC
                                                        └──────→ TCP fallback
```

Прокси извлекает DC ID из MTProto obfuscation init-пакета, выбирает маршрут и
перенаправляет зашифрованный поток в соответствующий Telegram DC.

<p align="center">
  <img width="900" alt="Схема работы TG WS Proxy" src="./images/workflow.png" />
</p>

## Документация

- [Статус и ограничения Rust-порта](./RUST_PORT.md)
- [Сборка из исходников](./BuildFromSource.md)
- [Docker](./README.docker.md)
- [Cloudflare Worker](./CfWorker.md)
- [Cloudflare-домен (CfProxy)](./CfProxy.md)
- [Fake TLS + upstream в Nginx](./FakeTlsNginx.md)
- [Тестовые DC Telegram](./TestDc.md)
- [Файлы конфигурации tray](./TrayConfig.md)
- [Руководство для контрибьюторов](./CONTRIBUTING.md)

### Инструкции по ОС

- [Windows](./README.windows.md)
- [macOS](./README.macos.md)
- [Linux](./README.linux.md)

## Происхождение

Rust-порт основан на проекте
[Flowseal/tg-ws-proxy](https://github.com/Flowseal/tg-ws-proxy). Python-код
сохранён в репозитории как эталон поведения и для legacy frontend.

## Лицензия

[MIT License](../LICENSE)
