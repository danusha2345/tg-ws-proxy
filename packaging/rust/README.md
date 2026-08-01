# TG WS Proxy — Rust

Tray проверен на Windows 11. macOS, Linux и Windows ARM64 проходят CI, но им
ещё полезны live-проверки на реальном железе, включая direct WebSocket,
Worker/CfProxy, TCP fallback, Fake TLS, sleep/resume и длительную работу.

## Содержимое

- `tg-ws-proxy` — консольный прокси;
- `tg-ws-proxy-desktop` — нативное tray-приложение;
- `tg-ws-proxy-musl` — статический Linux CLI, если он есть в архиве;
- `RUST_PORT.md` — статус порта и известные ограничения;
- `LICENSE` — лицензия MIT.

На Windows бинарники имеют расширение `.exe`.

## Запуск

CLI печатает ссылку `tg://proxy` после успешного запуска:

```text
tg-ws-proxy --secret-file /path/to/persistent/secret
```

Tray использует совместимый `TgWsProxy/config.json` и предоставляет основные
действия для запуска, настройки и подключения Telegram. На Windows после
запуска показывается сообщение о работе в системном трее, а настройки и логи
открываются в Блокноте, если системная файловая ассоциация отсутствует.
Через меню tray можно скачать стабильное обновление из GitHub Releases;
скачанный файл проверяется по `SHA256SUMS.txt`.

Не публикуйте secret и не прикладывайте его к отчёту об ошибке.

Актуальная документация, сборки и баг-трекер:

- https://github.com/danusha2345/tg-ws-proxy
- https://github.com/danusha2345/tg-ws-proxy/releases
- https://github.com/danusha2345/tg-ws-proxy/issues
