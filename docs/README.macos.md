# TG WS Proxy для macOS

> [!WARNING]
>
> Универсальная macOS-сборка проходит CI, но ещё нуждается в пользовательской
> проверке на реальных Intel и Apple Silicon устройствах.

## Готовая сборка

Откройте [Releases](https://github.com/danusha2345/tg-ws-proxy/releases) и
скачайте `TgWsProxy_macos_universal.dmg`. Сборка содержит Intel и Apple Silicon
варианты и рассчитана на macOS 11 или новее.

Приложение пока не подписано сертификатом Apple и не notarized. При первом
запуске macOS может потребовать подтвердить открытие в разделе
**Системные настройки → Конфиденциальность и безопасность**.

В релизе также есть `TgWsProxy_macos_universal.tar.gz` с отдельными CLI и
tray-бинарниками.
Пункт обновления скачивает стабильный DMG напрямую с GitHub, проверяет SHA-256
и открывает его системным приложением.

## Сборка из исходников

Установите Xcode Command Line Tools и [Rust](https://rustup.rs/):

```bash
xcode-select --install
git clone https://github.com/danusha2345/tg-ws-proxy.git
cd tg-ws-proxy
cargo build --release --locked --features desktop --bins
./target/release/tg-ws-proxy-desktop
```

CLI с постоянным secret:

```bash
./target/release/tg-ws-proxy \
  --secret-file "$HOME/Library/Application Support/tg-ws-proxy/secret"
```

## Настройка Telegram Desktop

Откройте напечатанную или скопированную ссылку `tg://proxy`. Для ручной
настройки добавьте MTProto-прокси:

- сервер: `127.0.0.1`;
- порт: `1443`;
- secret: значение из ссылки.
