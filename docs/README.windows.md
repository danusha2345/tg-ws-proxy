# TG WS Proxy для Windows

> [!WARNING]
>
> Rust-версия пока тестовая и не проходила полноценную проверку на реальных
> пользовательских окружениях.

## Готовая сборка

Откройте [Releases](https://github.com/danusha2345/tg-ws-proxy/releases) и
скачайте файл для своей системы:

- `TgWsProxy_windows_x64.exe` — Windows 10/11 x64;
- `TgWsProxy_windows_arm64.exe` — Windows 11 ARM64.

Rust-сборки для Windows 7 не выпускаются. Бинарники пока не имеют цифровой
подписи, поэтому Windows SmartScreen может показать предупреждение.

После запуска приложение появится в системном трее. Через меню можно открыть
или скопировать ссылку `tg://proxy`, перезапустить прокси, открыть настройки и
логи.

В релизе также доступны `tg-ws-proxy_cli_windows_*.exe` и ZIP-архивы с CLI,
tray-приложением, лицензией и документацией.

## Сборка из исходников

Установите [Rust](https://rustup.rs/), затем в PowerShell:

```powershell
git clone https://github.com/danusha2345/tg-ws-proxy.git
Set-Location tg-ws-proxy
cargo build --release --locked --features desktop --bins
.\target\release\tg-ws-proxy-desktop.exe
```

CLI с постоянным secret:

```powershell
.\target\release\tg-ws-proxy.exe `
  --secret-file "$env:LOCALAPPDATA\tg-ws-proxy\secret"
```

## Настройка Telegram Desktop

Откройте напечатанную или скопированную ссылку `tg://proxy`. Для ручной
настройки добавьте MTProto-прокси:

- сервер: `127.0.0.1`;
- порт: `1443`;
- secret: значение из ссылки.
