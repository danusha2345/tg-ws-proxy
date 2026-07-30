# TG WS Proxy для Android

Android-клиент запускает то же Rust-ядро локально на телефоне и передаёт
Telegram ссылку на MTProto-прокси `127.0.0.1`. Отдельный сервер для базового
режима не нужен.

> [!WARNING]
>
> Android-версия экспериментальная. Сборка проверена статически и в CI, но
> запуск с Telegram на реальном телефоне ещё требуется проверить. Не считайте
> её стабильной до таких тестов.

## Установка

На [странице Releases](https://github.com/danusha2345/tg-ws-proxy/releases)
скачайте:

- `TgWsProxy_android_arm64-v8a.apk` — большинство современных телефонов;
- `TgWsProxy_android_universal.apk` — универсальная сборка для ARM и x86_64.

Разрешите установку из выбранного браузера или файлового менеджера. Первый
тестовый релиз подписан тестовым ключом GitHub Actions; при смене ключа
следующую версию может потребоваться установить заново.

## Использование

1. Откройте приложение и разрешите уведомления.
2. Нажмите «Запустить прокси».
3. Дождитесь состояния «Работает».
4. Нажмите «Подключить Telegram».
5. Подтвердите добавление прокси в Telegram.

Прокси работает через Android Foreground Service. Пока он запущен, Android
показывает постоянное уведомление с кнопкой остановки. Это ожидаемое поведение:
без foreground service система не гарантирует работу локального listener в
фоне.

Если прошивка агрессивно ограничивает фоновые приложения, разрешите TG WS
Proxy работу без ограничений батареи. Особенно это актуально для Xiaomi,
Huawei, Honor и некоторых Samsung.

## Возможности первой версии

- Rust MTProto/WebSocket proxy на `127.0.0.1`;
- постоянное уведомление и запуск в фоне;
- direct WebSocket, Worker/CfProxy и TCP fallback;
- Fake TLS и masking upstream;
- генерация и защищённое хранение secret через Android Keystore;
- состояние, счётчики подключений и объём трафика;
- открытие Telegram и копирование `tg://proxy`;
- просмотр, очистка и отправка ротируемого лога;
- интерфейс на русском и английском.

Изменённые параметры применяются при следующем запуске прокси. Перед
редактированием настроек остановите текущий listener.

## Сборка

Нужны Android SDK 36, NDK `28.2.13676358`, JDK 17+ и Rust:

```bash
rustup target add \
  aarch64-linux-android \
  armv7-linux-androideabi \
  x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked

cd android
ANDROID_SDK_ROOT="$HOME/Android/Sdk" \
ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/28.2.13676358" \
./gradlew testDebugUnitTest lintDebug assembleDebug
```

Gradle сам собирает JNI-библиотеки из `crates/android-bridge`.

## Что проверить

- подключение официального Telegram Android к `127.0.0.1`;
- сообщения, фото, видео и большие файлы;
- Wi-Fi и мобильную сеть;
- переключение между сетями;
- работу при выключенном экране;
- direct WebSocket, Worker/CfProxy, TCP fallback и Fake TLS;
- длительную работу и поведение энергосбережения.

В Issue укажите модель телефона, версию Android, маршрут и приложите лог без
secret.
