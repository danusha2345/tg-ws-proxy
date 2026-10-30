# Собственный Worker без VPS

Начиная с Rust 1.15.0 / Android 0.5.0 Worker **не включён по умолчанию**:
используется бесплатный CF-пул, для медиа — HTTP/2 `/api`.
[Описание и ограничения](../docs/FREE_CF_TRANSPORT_20261030.md).
Этот каталог сохранён для тех, кто явно выбирает собственный Worker.


`worker.mjs` поддерживает HTTP chunk relay и legacy WebSocket к Telegram TCP/443.
HTTP relay сохраняет отдельные TCP-сессии в общем SQLite-backed Durable Object.
Legacy relay сохраняет блоки 8 KiB. Новый range relay согласует размер
до 32 KiB и непрерывный ACK; клиенты загружают диапазоны параллельно,
HTTP-соединения не переиспользуются. Очередь range relay ограничена 512 KiB.
Свой сервер и покупка домена не нужны: можно использовать адрес `workers.dev`.
Worker разрешает только фиксированный список production/test DC Telegram.
Произвольные IP, имена и порты не поддерживаются.

## Публикация

Нужен Cloudflare API token с правом **Workers Scripts Write** для своего
аккаунта. Токен передаётся Wrangler через окружение `CLOUDFLARE_API_TOKEN`;
его нельзя добавлять в репозиторий или в `wrangler.toml`.

Из этого каталога выполните `npx wrangler deploy`. Если Worker с именем
`tg-ws-bridge` уже существует, сначала проверьте, что он принадлежит этому
проекту: команда обновляет существующий скрипт.

## Подключение

В Android укажите выданный домен в поле **Домены Cloudflare Worker**.
В Windows заполните и включите поле Worker в настройках. Для CLI:

```bash
tg-ws-proxy --cfproxy-worker-domain WORKER.ACCOUNT.workers.dev --no-cfproxy
```

После проверки своего Worker отключите **Резервный маршрут Cloudflare**,
если не хотите использовать чужие домены. Этот переключатель не отключает
свои Worker-маршруты. Прямой Telegram WebSocket остаётся доступным независимо
от него.

Если WSS/TLS не проходит, а обычный WS работает, включите **CF и Worker без
TLS (порт 80)** / `--no-secure`. Это явный выбор транспорта: проверка
сертификатов WSS не отключается. MTProto-шифрование между Telegram-клиентом
и Telegram сохраняется; внешний TLS-слой при WS отсутствует.

## Проверка

`GET /health` проверяет обработчик Worker, но не доступность Telegram.
Для проверки настоящего MTProto-обмена используйте `tests/support/mtproto_probe.py`
из корня проекта с зависимостями, указанными в начале этого файла:

```bash
python3 tests/support/mtproto_probe.py \
  --domain WORKER.ACCOUNT.workers.dev --ip WORKER.ACCOUNT.workers.dev \
  --path '/apiws?dst=149.154.167.51&dc=2' --dc 2
```

Для WS на порту 80 добавьте `--no-secure`. Проверка заканчивается на
`ServerDHParamsOk`: аккаунт, сообщения и готовый auth key не используются.
Probe также умеет проверять локальный MTProxy через `--proxy-host`,
`--proxy-port` и `--secret-file` с исходным 32-символьным HEX-secret.

## Ограничения

Свой Worker устраняет зависимость от владельцев общего доменного пула,
но остаётся зависимым от Cloudflare. Несколько Workers в одном аккаунте
делят его лимиты и не являются независимыми сетевыми маршрутами.
На Workers Free действует лимит 100 000 входящих запросов в сутки на аккаунт;
WebSocket-сообщения не следует путать с отдельными входящими HTTP-запросами.
Платный тариф этот проект автоматически не включает.

Worker не требует отдельной авторизации, поэтому любой знающий домен может
использовать его для разрешённых адресов Telegram. Это не универсальный TCP
relay. Вход ограничен бинарными кадрами до 256 KiB и очередью до 1 MiB на
соединение. Публиковать адрес как массовый публичный прокси не рекомендуется.

Документация Cloudflare:
[TCP sockets](https://developers.cloudflare.com/workers/runtime-apis/tcp-sockets/),
[лимиты](https://developers.cloudflare.com/workers/platform/limits/),
[binaryType](https://developers.cloudflare.com/workers/configuration/compatibility-flags/#websocket-standard-binary-type).
