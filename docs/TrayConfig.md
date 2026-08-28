# Файлы конфигурации Tray-приложения

Rust tray (`cargo run --features desktop --bin tg-ws-proxy-desktop`) использует
тот же формат и сохраняет неизвестные поля, поэтому один config можно
поочерёдно открывать legacy и Rust frontend. На Unix Rust сужает права файла до
`0600`, поскольку в нём хранится secret.

Tray-приложение хранит данные в:

- **Windows:** `%APPDATA%/TgWsProxy`
- **macOS:** `~/Library/Application Support/TgWsProxy`
- **Linux:** `~/.config/TgWsProxy` (или `$XDG_CONFIG_HOME/TgWsProxy`)

```json
{
  "host": "127.0.0.1",
  "port": 1443,
  "secret": "...",
  "dc_ip": [
    "2:149.154.167.220",
    "4:149.154.167.220"
  ],
  "verbose": false,
  "buf_kb": 256,
  "pool_size": 4,
  "log_max_mb": 5.0,
  "check_updates": true,
  "cfproxy": true,
  "cfproxy_user_domain_enabled": false,
  "cfproxy_user_domain": [],
  "cfproxy_worker_enabled": false,
  "cfproxy_worker_domain": [],
  "force_test_dc": false,
  "appearance": "auto"
}
```

Поля `cfproxy_user_domain_enabled` и `cfproxy_worker_enabled` управляют
использованием сохранённых доменов отдельно от самих списков. Для старого
config без этих флагов непустой список автоматически считается включённым.

Ключ `check_updates`: при `true` Rust tray проверяет стабильные `rust-v*`
релизы на GitHub. Через меню можно скачать подходящий asset; перед запуском он
сверяется с опубликованным `SHA256SUMS.txt`.
На Windows в конфиге может быть `autostart` (автозапуск при входе в систему).

Rust tray исполняет `check_updates`, сохраняет `autostart` без изменений, но
пока не управляет автозапуском.
