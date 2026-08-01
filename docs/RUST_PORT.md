# Rust-порт

Ветка `rust-port` переносит консольное ядро TG WS Proxy с Python на Rust. Цель
порта — сохранить MTProto/WebSocket-совместимость, одновременно закрыв ошибки
жизненного цикла соединений и ограничив потребление ресурсов на недоверенном
входе.

> [!WARNING]
>
> Tray проверен на Windows 11. Остальные desktop targets проверяются сборкой и
> тестами в CI, но ещё нуждаются в live-проверках на реальном железе, включая
> direct WebSocket, Worker/CfProxy, TCP fallback и Fake TLS.

## Текущее состояние

В Rust реализованы:

- разбор 64-байтового MTProto obfuscation init и выбор DC;
- AES-CTR relay в обоих направлениях;
- прямой TLS WebSocket, Cloudflare Worker/CfProxy и TCP fallback;
- Fake TLS, HAProxy PROXY protocol v1 и пул предварительных соединений;
- ограничение размера WebSocket-сообщения и числа одновременных клиентов;
- ротация логов и совместимые основные параметры CLI;
- feature-gated нативный tray для Linux, Windows и macOS с единым Rust runtime;
- Android JNI frontend с foreground service, настройками и логами;
- Docker-образ с запуском от непривилегированного пользователя;
- обновление desktop-приложения из стабильных GitHub Releases с проверкой
  опубликованной SHA-256 суммы.

Python-исходники пока сохранены как эталон поведения и для полного legacy tray
с GUI-настройками и autostart. Rust updater намеренно принимает только наши
стабильные теги `rust-v*` и не устанавливает upstream Python-релизы.

## Сборка и проверка

Нужен Rust `1.85` или новее:

```bash
cargo build --release --locked
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --features desktop
cargo clippy --all-targets --features desktop -- -D warnings
```

Запуск с постоянным secret:

```bash
./target/release/tg-ws-proxy \
  --host 127.0.0.1 \
  --port 1443 \
  --secret-file "$HOME/.local/state/tg-ws-proxy/secret"
```

При первом запуске файл создаётся автоматически. Не публикуйте его и не
добавляйте в Git.

Для просмотра всех параметров:

```bash
./target/release/tg-ws-proxy --help
```

Нативный tray:

```bash
cargo run --locked --features desktop --bin tg-ws-proxy-desktop
```

Он использует совместимый `TgWsProxy/config.json`, показывает состояние
listener и предоставляет действия «Открыть в Telegram», «Копировать ссылку»,
«Перезапустить», «Открыть настройки», «Открыть логи» и «Выйти». Настройки пока
открываются в системном JSON-редакторе; на Windows при отсутствии файловой
ассоциации настройки и логи открываются через Блокнот. После запуска Windows
показывает сообщение, что приложение работает в системном трее и значок может
находиться среди скрытых значков. Пункт обновления скачивает подходящий asset
стабильного `rust-v*` релиза, проверяет его по `SHA256SUMS.txt` и открывает
системную установку; на Windows новая версия запускается после завершения
текущей. Встроенного GUI-редактора пока нет. Обычный Restart перечитывает
proxy-параметры и язык, но изменения
`verbose` и `log_max_mb` применяются только после полного перезапуска desktop
process.

## Docker

Образ собирается в multi-stage режиме, а в runtime попадает только release
binary, `ca-certificates` и `tini`:

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

По умолчанию secret хранится в `/data/secret`, поэтому не меняется после
перезапуска или пересоздания контейнера с тем же volume. Для удалённого
хоста обязательно укажите адрес, который должен попасть в ссылку Telegram:

```bash
-e TG_WS_PROXY_ADVERTISE_HOST=proxy.example.com
```

## Исправления относительно Python-реализации

- TLS-сертификат и имя сервера проверяются; WebSocket upgrade проходит
  стандартную RFC 6455-проверку.
- Continuation frames собираются библиотекой WebSocket и не теряются между
  AES-CTR блоками.
- CLOSE/EOF закрывают транспорт; устаревшие pooled-соединения не выдаются
  новому клиенту.
- Невалидный init не удерживает соединение бесконечно, а размеры входных
  сообщений и число клиентов имеют верхние границы.
- Ошибочные огромные значения pool/frame/connection/log-backup отклоняются до
  запуска вместо panic, массового connect storm или зависания ротации.
- Ошибка отправки через pooled WebSocket не отменяет TCP fallback.
- Redirect и timeout учитываются раздельно: временный timeout не создаёт
  постоянный redirect-ban для DC.
- Worker-домены перемешиваются локально, а один предварительно подключённый
  socket на DC разделяет общий пул доменов и сразу восполняется после выдачи.
- `OPTI` исключён из случайно генерируемых первых четырёх байтов relay init.
- `--advertise-host` отделён от bind-адреса, поэтому Docker не публикует в
  `tg://proxy` внутренний bridge IP.
- Secret/config/log files создаются с правами `0600` на Unix, а legacy config
  с более широкими правами сужается при первом чтении.

Для Fake TLS `--masking-upstream` должен указывать на отдельный HTTPS origin.
Совпадающие Fake TLS и masking domains, а также очевидная локальная петля на
адрес listener отклоняются. Петлю через внешний reverse proxy, CDN или NAT
процесс надёжно распознать не может, поэтому такой upstream нужно проверять при
настройке.

## Связь с upstream issues

| Issue | Что учтено в порте |
| --- | --- |
| [#920](https://github.com/Flowseal/tg-ws-proxy/issues/920) — таймауты при параллельных WS | Изоляция сессий, ограничение клиентов, возраст pooled-соединений и раздельные cooldown |
| [#1161](https://github.com/Flowseal/tg-ws-proxy/issues/1161) — большие файлы через Worker | Worker route отправляет поток bounded chunks до 64 KiB, не собирая большой MTProto packet в одно WS-сообщение; внешние лимиты самого Worker порт не отменяет |
| [#1155](https://github.com/Flowseal/tg-ws-proxy/issues/1155) — media/WARP | Маршруты DC остаются настраиваемыми; WARP-специфичный обход не добавлен без подтверждённой причины |
| [#621](https://github.com/Flowseal/tg-ws-proxy/issues/621) — resume после сна | Устаревшие pooled-сокеты закрываются; поведение tray после sleep/resume требует отдельной проверки |

## Границы совместимости

- Современная Rust-сборка ориентирована на Windows 10+, актуальные macOS и
  Linux. Windows 7 требует отдельного legacy toolchain и не заявлен как
  поддерживаемый этой веткой.
- Cloudflare и Telegram могут менять внешнее поведение независимо от кода.
  Перед релизом нужны live smoke-тесты direct WS, Worker/CfProxy и TCP
  fallback.
- В отличие от legacy Python runtime, Rust пока не обновляет CfProxy domain
  pool с GitHub каждый час: в binary встроен проверенный список из 20 доменов,
  который можно заменить через `--cfproxy-domain`. При ротации upstream
  потребуется новый build или явный override.
- Rust tray покрывает основные runtime-действия и обновление из GitHub, но
  полный GUI-редактор, Windows autostart/portable mode и sleep/resume
  интеграция пока остаются legacy-возможностями.
