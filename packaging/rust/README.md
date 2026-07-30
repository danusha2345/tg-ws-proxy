# TG WS Proxy — тестовая Rust-сборка

Эта сборка пока не проходила полноценное пользовательское тестирование.
Нужны проверки CLI и tray на Windows, macOS и Linux, включая direct
WebSocket, Worker/CfProxy, TCP fallback, Fake TLS, sleep/resume и длительную
работу.

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
действия для запуска, настройки и подключения Telegram.

Не публикуйте secret и не прикладывайте его к отчёту об ошибке.

Актуальная документация, сборки и баг-трекер:

- https://github.com/danusha2345/tg-ws-proxy
- https://github.com/danusha2345/tg-ws-proxy/releases
- https://github.com/danusha2345/tg-ws-proxy/issues
