# TG WS Proxy для Docker (Rust)

## Установка из исходников

```bash
# Скачиваем репозиторий
git clone https://github.com/danusha2345/tg-ws-proxy.git

# Переходим в папку с проектом
cd tg-ws-proxy
git switch rust-port

# Собираем образ
docker build -t tg-ws-proxy:rust .

# Создаём volume для постоянного secret
docker volume create tg-ws-proxy-data

# Запускаем контейнер от непривилегированного пользователя
docker run -d \
  --name tg-ws-proxy \
  --restart unless-stopped \
  -p 1443:1443 \
  -v tg-ws-proxy-data:/data \
  tg-ws-proxy:rust

# Получаем ссылку для подключения
docker logs tg-ws-proxy 2>&1 | grep 'tg://proxy'
```

После выполнения последней команды вы увидите ссылку вида:

```text
tg://proxy?server=127.0.0.1&port=1443&secret=dd68f127db1d...
```

Secret создаётся один раз в `/data/secret` с закрытыми правами доступа и
повторно используется после рестарта. Не удаляйте volume, если хотите
сохранить прежнюю ссылку подключения.

## Настройка параметров

Все настройки задаются переменными окружения при запуске контейнера:

| Переменная | Описание | По умолчанию |
| --- | --- | --- |
| `TG_WS_PROXY_HOST` | Bind-адрес контейнера | `0.0.0.0` |
| `TG_WS_PROXY_PORT` | Порт внутри контейнера | `1443` |
| `TG_WS_PROXY_ADVERTISE_HOST` | Адрес в `tg://proxy` | `127.0.0.1` |
| `TG_WS_PROXY_SECRET_FILE` | Файл с постоянным secret | `/data/secret` |
| `TG_WS_PROXY_SECRET` | Явный 32-значный hex-secret вместо файла | не задан |
| `TG_WS_PROXY_DC_IPS` | Пары `DC:IP` через пробел | DC 2 и 4 |
| `TG_WS_PROXY_CF_WORKER` | Домен Cloudflare Worker | не задан |

Пример с ручным указанием секрета:

```bash
docker run -d \
  --name tg-ws-proxy \
  --restart unless-stopped \
  -p 1443:1443 \
  -e TG_WS_PROXY_SECRET="00112233445566778899aabbccddeeff" \
  tg-ws-proxy:rust
```

Для генерации secret можно использовать:

```bash
openssl rand -hex 16
```

Если контейнер работает не на том же компьютере, где запущен Telegram,
задайте публичный IP или DNS-имя отдельно от bind-адреса:

```bash
docker run -d \
  --name tg-ws-proxy \
  --restart unless-stopped \
  -p 1443:1443 \
  -v tg-ws-proxy-data:/data \
  -e TG_WS_PROXY_ADVERTISE_HOST=proxy.example.com \
  tg-ws-proxy:rust
```

Дополнительные параметры CLI можно передать после имени образа. Например:

```bash
docker run --rm \
  -p 1443:1443 \
  -v tg-ws-proxy-data:/data \
  tg-ws-proxy:rust \
  --no-cfproxy \
  --dc-ip 4:149.154.167.220
```

## Настройка Telegram Desktop

1. В Telegram откройте **Настройки** → **Продвинутые настройки** →
   **Тип подключения** → **Прокси**.
2. Добавьте прокси:
   - **Тип:** MTProto
   - **Сервер:** `127.0.0.1` (или переопределенный вами)
   - **Порт:** `1443` (или переопределенный вами)
   - **Secret:** из настроек или логов
