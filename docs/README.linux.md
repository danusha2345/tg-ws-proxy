# TG WS Proxy для Linux

> [!WARNING]
>
> Linux-сборки проходят CI, но ещё нуждаются в расширенной проверке в разных
> desktop environments и дистрибутивах.

## Готовые сборки

На [странице Releases](https://github.com/danusha2345/tg-ws-proxy/releases)
публикуются:

| Архитектура | Бинарник | Пакеты |
| --- | --- | --- |
| x86_64 | `TgWsProxy_linux_amd64` | `.deb`, `.rpm` |
| ARM64 | `TgWsProxy_linux_arm64` | `.deb`, `.rpm` |

Основной бинарник запускает tray. Отдельные `tg-ws-proxy_cli_linux_*` запускают
только консольный прокси; варианты с суффиксом `_musl` собраны статически.

После скачивания отдельного бинарника:

```bash
chmod +x TgWsProxy_linux_amd64
./TgWsProxy_linux_amd64
```

Для работы tray нужен запущенный пользовательский D-Bus и реализация
StatusNotifierItem/AppIndicator в окружении рабочего стола.
Пункт обновления скачивает `.deb` или `.rpm` стабильного релиза, проверяет
SHA-256 и открывает пакет системным установщиком.

## Сборка из исходников

Установите [Rust](https://rustup.rs/), затем:

```bash
git clone https://github.com/danusha2345/tg-ws-proxy.git
cd tg-ws-proxy
cargo build --release --locked --features desktop --bins
./target/release/tg-ws-proxy-desktop
```

CLI с постоянным secret:

```bash
./target/release/tg-ws-proxy \
  --secret-file "$HOME/.local/state/tg-ws-proxy/secret"
```

## Настройка Telegram Desktop

Откройте напечатанную или скопированную ссылку `tg://proxy`. Для ручной
настройки добавьте MTProto-прокси:

- сервер: `127.0.0.1`;
- порт: `1443`;
- secret: значение из ссылки.
