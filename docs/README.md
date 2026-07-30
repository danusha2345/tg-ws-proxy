# TG WS Proxy — Rust

[![Boosty](https://img.shields.io/badge/Boosty-Поддержать-FF7143?style=for-the-badge&logo=boosty&logoColor=white)](https://boosty.to/danusha/donate)

Локальный MTProto-прокси для Telegram Desktop и Android. Он перенаправляет
трафик через TLS WebSocket и автоматически использует доступный резервный
маршрут, не требуя отдельного пользовательского сервера для базового режима.

> [!WARNING]
>
> Rust-ветка пока не проходила полноценное пользовательское тестирование на
> реальных окружениях и не готова считаться стабильным релизом. Нужны тесты на
> Windows, macOS, Linux и Android, а также live-проверки direct WebSocket,
> Worker/CfProxy, TCP fallback, Fake TLS, tray и Android foreground service.
> Используйте её как тестовую сборку и
> [сообщайте о найденных проблемах в Issues](https://github.com/danusha2345/tg-ws-proxy/issues).

## Что уже реализовано

- MTProto obfuscation init и AES-CTR relay в обоих направлениях;
- direct TLS WebSocket, domain fronting, Cloudflare Worker/CfProxy и TCP
  fallback;
- Fake TLS и HAProxy PROXY protocol v1;
- пул соединений, лимиты клиентов и размера WebSocket-сообщений;
- постоянный secret, ротация логов и Docker-образ без root;
- опциональный нативный tray для Windows 10+, актуальных macOS и Linux;
- Android-приложение с foreground service, настройками и просмотром логов.

Подробный статус, исправленные ошибки и известные ограничения описаны в
[документе о Rust-порте](./RUST_PORT.md).

## Готовые тестовые сборки

Собственные Rust-сборки публикуются на
[странице Releases](https://github.com/danusha2345/tg-ws-proxy/releases).

| Система | Артефакт |
| --- | --- |
| Windows 10+ x64 | `TgWsProxy_windows_x64.exe` |
| Windows 11 ARM64 | `TgWsProxy_windows_arm64.exe` |
| macOS 11+ Intel / Apple Silicon | `TgWsProxy_macos_universal.dmg` |
| Linux x86_64 | `TgWsProxy_linux_amd64`, `.deb` или `.rpm` |
| Linux ARM64 | `TgWsProxy_linux_arm64`, `.deb` или `.rpm` |
| Android ARM64 | `TgWsProxy_android_arm64-v8a.apk` |
| Android universal | `TgWsProxy_android_universal.apk` |

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

### Нативный tray

```bash
cargo run --release --locked \
  --features desktop \
  --bin tg-ws-proxy-desktop
```

Tray использует тот же Rust runtime. Полный GUI-редактор, auto-update,
Windows autostart и portable mode пока остаются возможностями legacy
Python-версии.

### Android

Android-приложение собирает Rust-ядро через JNI и запускает локальный listener
в foreground service. Инструкция по установке, сборке и тестированию:
[TG WS Proxy для Android](./README.android.md).

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

Особенно полезны проверки на Windows, macOS, разных окружениях Linux и Android:

1. запуск CLI и tray;
2. подключение Telegram по напечатанной ссылке;
3. сообщения, фото, видео и большие файлы;
4. direct WebSocket, Worker/CfProxy и TCP fallback;
5. Fake TLS, sleep/resume и длительная работа.
6. Android: выключенный экран, смена Wi-Fi/мобильной сети и энергосбережение.

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
- [Android](./README.android.md)
- [Руководство для контрибьюторов](./CONTRIBUTING.md)

### Инструкции по ОС

- [Windows](./README.windows.md)
- [macOS](./README.macos.md)
- [Linux](./README.linux.md)
- [Android](./README.android.md)

## Происхождение

Rust-порт основан на проекте
[Flowseal/tg-ws-proxy](https://github.com/Flowseal/tg-ws-proxy). Python-код
сохранён в репозитории как эталон поведения и для legacy frontend.

## Лицензия

[MIT License](../LICENSE)
