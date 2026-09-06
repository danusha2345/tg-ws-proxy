# TG WS Proxy для Windows

> Windows x64 и ARM64 собираются в CI. Восстановление окна проверяется
> нативным Windows-тестом; полная проверка интерфейса на реальном ПК остаётся
> отдельным этапом.

## Готовая сборка

Откройте [Releases](https://github.com/danusha2345/tg-ws-proxy/releases) и
скачайте файл для своей системы:

- `TgWsProxy_windows_x64.exe` — Windows 10/11 x64;
- `TgWsProxy_windows_arm64.exe` — Windows 11 ARM64.

Rust-сборки для Windows 7 не выпускаются. Бинарники пока не имеют цифровой
подписи, поэтому Windows SmartScreen может показать предупреждение.

После запуска открывается окно подключения и настроек. Закрытие крестиком
скрывает окно в системный трей и оставляет прокси работающим. Двойной щелчок
по значку или пункт «Открыть TG WS Proxy» возвращает окно, в том числе после
минимизации. Для завершения приложения выберите «Выйти».
Если значок не виден рядом с часами, откройте скрытые значки стрелкой `^`.
Через меню можно открыть или скопировать ссылку `tg://proxy`, перезапустить
прокси, открыть настройки и логи. Если для `.json` или `.log` не назначено
приложение, файл откроется в Блокноте.
Пункт обновления скачивает новый стабильный Windows asset напрямую с GitHub,
проверяет SHA-256 и запускает его после завершения текущей версии.

В релизе также доступны `tg-ws-proxy_cli_windows_*.exe` и ZIP-архивы с CLI,
tray-приложением, лицензией и документацией.

## Сборка из исходников

Установите [Rust](https://rustup.rs/), затем в PowerShell:

```powershell
git clone --branch rust-port https://github.com/danusha2345/tg-ws-proxy.git
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
