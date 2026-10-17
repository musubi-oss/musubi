# Матрица паритета: Musubi против Pragma / Nakama / PlayFab / AccelByte

Снимок состояния бэкенда на `main` (октябрь 2026). Проверено по коду, не по планам: каждая строка
со статусом ✅ или 🟡 указывает крейт/маршрут. Обозначения: ✅ есть · 🟡 частично · ❌ нет · — не
применимо. Колонки конкурентов — из части 2 плана (`game-backend-plan/02-matrica-i-dyry.md`),
Pragma Engine и AccelByte добавлены по их публичным фичлистам.

В конце — упорядоченная очередь работ: что закрываем, в каком порядке и почему.

## 1. Платформа

| Фича | Musubi | Pragma | Nakama OSS / Ent | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Лицензия, self-host | ✅ Apache-2.0, один бинарь | source-available, лицензия на студию | Apache-2.0 / Ent платно | SaaS | SaaS + enterprise |
| Кластер из коробки | ✅ `musubi-cluster` (SWIM + Raft-директория, шина) | ✅ | ❌ / ✅ | ✅ | ✅ |
| Мультирегион | ✅ регионы нод, размещение миров по региону (`worlds`) | 🟡 | ❌ / 🟡 | ✅ | ✅ |
| Геороутинг клиента к ближайшему региону | ❌ (нет geoip / latency-probe на входе) | 🟡 | ❌ | ✅ | ✅ |
| Окружения dev → stage → prod с промоушеном конфига/контента | 🟡 контент-бандлы: `GET /v1/admin/content` экспорт определений (валюты, предметы, награды, прогрессии, квесты, витрина, купоны, очереди рейтинга), `POST …/import` с dry-run diff'ом и prune, история версий и откат; окружения — отдельные деплои, конфиг кластера один | ✅ (Pragma environments) | ❌ | ✅ | ✅ |
| Мультипроект (несколько игр на кластере) | ❌ (один tenant) | ✅ | ❌ | ✅ (titles) | ✅ (namespaces) |
| Скоуп-ключи сервер/админ | ✅ `server_keys` role server\|admin, аудит мутаций | ✅ | 🟡 один HTTP key | ✅ | ✅ |
| RBAC операторов консоли (роли, SSO операторов) | 🟡 роли ключей server / admin / support / liveops / ops (чтение всем, запись по области, 403 с маршрутом); SSO операторов — нет | ✅ | ❌ / ✅ | ✅ | ✅ |
| Квоты и rate-limit per key / per account | 🟡 лимиты auth/ip, storage per-user; per-key нет | ✅ | ❌ | ✅ | ✅ |
| Бэкап / restore данных игрока точечно | 🟡 export JSON (`/v1/admin/accounts/{id}/export`), restore нет | 🟡 | ❌ | ✅ | 🟡 |

## 2. Идентичность, данные, социалка

| Фича | Musubi | Pragma | Nakama | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Device / email+пароль / Steam / Google / Apple | ✅ `musubi-auth`, `http/auth.rs` | ✅ | ✅ | ✅ | ✅ |
| Generic OIDC (Discord, Twitch, Epic, Keycloak…) | ✅ `/v1/auth/oidc/{provider}`, discovery + JWKS | 🟡 | 🟡 | 🟡 | ✅ |
| Xbox Live (XSTS), PSN, Nintendo | ❌ | ✅ | ❌ (custom) | ✅ | ✅ |
| Passkeys / WebAuthn | ✅ | ❌ | ❌ | ❌ | ❌ |
| TOTP MFA + recovery codes | ✅ | ❌ | ❌ | ❌ | 🟡 |
| Magic-link вход | ❌ (есть verify/reset по токену, вход по ссылке нет) | ❌ | ❌ | ✅ | ❌ |
| Lockout, captcha при брутфорсе, suspicious login | ✅ `[session]`, `hook_auth_captcha`, `account.suspicious_login` | 🟡 | ❌ | 🟡 | 🟡 |
| Login queue при лимите онлайна | ✅ `POST /v1/auth/queue` (гейт в выдаче сессии) | ✅ | ❌ | ❌ | 🟡 |
| Линковка / слияние аккаунтов | ✅ link/unlink, `POST /v1/account/merge` с журналом | ✅ | ✅ (link) | ✅ | ✅ |
| Сессии: refresh-семейства, revoke, device list, IP | ✅ | ✅ | ✅ | ✅ | ✅ |
| Отправка e-mail (verify/reset) | 🟡 событие `email.*` в шину → вебхук/модуль; SMTP-провайдера нет | 🟡 | ❌ | ✅ | ✅ |
| Хранилище объектов (коллекции, права, версии, TTL) | ✅ `musubi-storage` | ✅ player data | ✅ | ✅ | ✅ |
| Запросы по полям | ✅ `list_where` (фильтр по JSON-полям) | 🟡 | 🟡 | ❌ | ❌ |
| Вторичные индексы / full-text | ❌ | ❌ | 🟡 | ❌ | ❌ |
| Live-подписка на изменения объектов | ❌ | ❌ | ❌ | ❌ | ❌ |
| Схемы player data и миграции версий | ❌ (объекты бессхемные) | ✅ | ❌ | ❌ | ❌ |
| Cloud save с разрешением конфликтов | 🟡 версии + CAS, слияние на клиенте | ❌ | 🟡 | ✅ | ✅ |
| Друзья, блок, рекомендации, недавние | ✅ `musubi-social` | ✅ | ✅ | ✅ | ✅ |
| Импорт друзей с платформ (Steam/PSN/Xbox) | ❌ | ✅ | ❌ | 🟡 | ✅ |
| Группы: роли-битмаска, казна, склад, XP/уровни, альянсы | ✅ | 🟡 | 🟡 | 🟡 | ✅ |
| Войны / челленджи кланов | ❌ (геймплейная логика поверх, ядро есть) | ❌ | ❌ | ❌ | ❌ |
| Чат: история, треды, реакции, правка, упоминания, модерация, slow-mode | ✅ `musubi-chat` | 🟡 | ✅ | 🟡 | ✅ |
| Перевод сообщений | ❌ | ❌ | ❌ | ❌ | ❌ |
| Голос | ✅ через Aurix (токены, позиционный канал мира, модерация) | ❌ | ❌ | ✅ | ❌ |
| Присутствие / статус | ✅ hub + шина (`online_remote`) | ✅ | ✅ | ❌ | ✅ |
| Пати, лобби, ready, команды | ✅ parties (home-node) + `/v1/lobbies` | ✅ | ✅ | ✅ | ✅ |
| Уведомления in-app | ✅ | ✅ | ✅ | ✅ | ✅ |
| Push FCM / APNs | ✅ `push.rs` | ❌ | ❌ | ✅ | ❌ |
| WebPush | ❌ | ❌ | ❌ | ❌ | ❌ |
| UGC (моды, карты, скины) | ❌ | ❌ | ❌ | ✅ | ✅ |
| GDPR export / delete с retention | ✅ | ✅ | ✅ | ✅ | ✅ |
| Возраст / родительский контроль / COPPA, приватность профиля | ❌ | ✅ | ❌ | 🟡 | ✅ |

## 3. Соревновательные и мета-системы

| Фича | Musubi | Pragma | Nakama (+Hiro) | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Лидерборды: операторы, сброс по cron, around | ✅ | 🟡 | ✅ | ✅ | ✅ |
| Лиги / дивизионы (promote/demote по тирам) | ✅ `League` в `musubi-leaderboard` | ❌ | ❌ | ❌ | ❌ |
| Турниры: окно, join_code, призы по рангам, командные | ✅ | 🟡 | ✅ | 🟡 | 🟡 |
| Брекеты (single/double elimination, swiss) | ❌ | ❌ | ❌ | ❌ | ❌ |
| Рейтинги Elo / Glicko-2 / OpenSkill, сезоны, decay | ✅ `rating.rs` | 🟡 | ❌ | ❌ | 🟡 |
| Достижения, квесты, стрики, энергия, прогрессия | ✅ `musubi-economy` | 🟡 | Hiro платно | ✅ | ✅ |
| Сезоны / баттл-пасс | ✅ | ❌ | Hiro | ✅ | ✅ |
| Валюты, инвентарь, стор (окна, бандлы, гейты, скидки, купоны, ротация) | ✅ | ✅ | 🟡 wallet | ✅ | ✅ |
| Крафт | ❌ (рецепты нет; есть consume/grant) | ✅ | Hiro | ❌ | ❌ |
| Гача с pity и публичным seed (commit-reveal) | ✅ | ❌ | Hiro | ❌ | ❌ |
| Аукционы, P2P-трейд, ордербук | ✅ | ❌ | ❌ | ❌ | ❌ |
| IAP: Apple / Google / Steam, уведомления, отзыв | ✅ `iap.rs` | ✅ | ✅ | ✅ | ✅ |
| IAP: PSN / Xbox / Nintendo / Epic | ❌ | ✅ | ❌ | ✅ | ✅ |
| Подписки с продлением / grace | ✅ `iap_products.kind = subscription` + `grace_secs`, продление по чеку и Apple V2 / Google RTDN, `GET /v1/iap/subscriptions` | 🟡 | ✅ | ✅ | ✅ |
| Entitlements: платформенные покупки / DLC → предметы | ❌ | ✅ | ❌ | ✅ | ✅ |
| Limited grants (лимит серверных грантов на игрока в окно) | ❌ | ✅ | ❌ | ❌ | ❌ |
| Жалобы на игроков общего вида + ревью | 🟡 только на сообщения чата | ✅ | ❌ | ✅ | ✅ |
| Баны (timed, shadow), kick, purge | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Античит: серверная валидация, EAC/BattlEye-интеграция | ❌ (только authoritative-матчи как модель) | 🟡 | ❌ | 🟡 | 🟡 |

## 4. Realtime, матчмейкинг, серверы

| Фича | Musubi | Pragma | Nakama | PlayFab | AccelByte |
|---|---|---|---|---|---|
| WebSocket, QUIC, WebTransport, UDP, WebRTC DataChannel | ✅ `musubi-transport` (все пять) | — | WS | WS/UDP | WS |
| Relay-матчи, host migration по RTT | ✅ | ❌ | ✅ relayed | ✅ | ❌ |
| Authoritative-матчи (акторы), снапшоты + журнал, failover, fencing | ✅ | ❌ (game instances на DS) | ✅ (без failover) | 🟡 | ❌ |
| Spectator, запись и экспорт реплеев | ✅ | ❌ | ❌ | ❌ | ❌ |
| Детерминированный rollback | ❌ | ❌ | ❌ | ❌ | ❌ |
| Матчмейкер: MMR-окно по времени, пинг, роли, команды, кроссплей, приоритет, ready-check, dodge | ✅ `QueueConfig` | ✅ | 🟡 query | ✅ | ✅ |
| Backfill в живой матч / сервер | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Game flow: пати → очередь → инстанс → результаты → награды | 🟡 звенья есть; единого «end-game report → рейтинги + награды + статы» маршрута нет | ✅ | ❌ | 🟡 | ✅ |
| Dedicated-серверы: провайдеры | ✅ local, docker, kubernetes (Agones-совместимый манифест), firecracker | 🟡 внешние | 🟡 FleetManager API | ✅ | ✅ AMS |
| Внешние оркестраторы как провайдеры (Edgegap, GameLift) | ❌ | — | ✅ | — | — |
| Автоскейл: буфер + прогноз по истории | ✅ `demand`/`scale` | ❌ | ❌ | ✅ | ✅ |
| Scale-to-zero флота | ❌ (min 0 задать можно, прогрев/холодный старт не отработан) | ❌ | ❌ | ✅ | ✅ |
| Build pipeline: canary / promote / rollback, vCPU-часы | ✅ | ❌ | ❌ | 🟡 | ✅ |
| SDK сервера: ready/health/labels/players/watch/shutdown | ✅ `/v1/gameserver/*` | ✅ | 🟡 | ✅ | ✅ |

## 5. Логика, LiveOps, инструменты

| Фича | Musubi | Pragma | Nakama | PlayFab | AccelByte |
|---|---|---|---|---|---|
| Серверная логика | ✅ WASM-модули (`musubi-runtime`): rpc, before/after, hook_*, match_*, schedule_* | Kotlin-плагины | Go/Lua/JS | CloudScript | ❌ |
| Hot reload модулей | ✅ `PUT /v1/admin/runtime/modules/{name}` | ❌ | ❌ | ✅ | — |
| Канареечный rollout модулей | ❌ (есть для билдов флота, не для модулей) | ❌ | ❌ | ❌ | — |
| Изоляция: fuel, память, дедлайн | ✅ | — | ❌ | ✅ | — |
| Локальная отладка: dev-сервер, фикстуры, replay запросов | 🟡 `musubi dev`; фикстур и записи/воспроизведения запросов нет | ✅ | 🟡 | 🟡 | — |
| Cron / schedule_-хуки | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Durable workflows / saga | ❌ | ❌ | ❌ | ✅ | ❌ |
| Flags, live events, аудитории, A/B | ✅ через Zanshin (кэш + мгновенный пуш `liveops.changed`) | ✅ | Satori платно | ✅ | ✅ |
| Аналитика событий | ✅ шина → Zanshin / вебхуки; клиентскую телеметрию пишет Zanshin SDK | ✅ | Satori | ✅ | ✅ |
| Вебхуки: HMAC, backoff, dead-letter, redeliver, test | ✅ | 🟡 | ❌ | ✅ | ✅ |
| Аудит админ-мутаций | ✅ `/v1/admin/audit` | ✅ | ❌ | ✅ | ✅ |
| Админ-API: аккаунты (поиск по identity, patch, ban, sessions, export), storage, chat-модерация, worlds/matches, конфиг | ✅ | ✅ portal | 🟡 | ✅ | ✅ |
| Контент-каталоги с версиями / item definitions | 🟡 items/products/tables через server API, без версий и промоушена | ✅ | ❌ | ✅ | ✅ |
| OpenAPI, CLI, миграция с Nakama | ✅ `openapi.json`, `musubi migrate-nakama` | 🟡 | 🟡 | ✅ | ✅ |
| IaC (Helm / Terraform) | ❌ | — | 🟡 | — | — |
| Prometheus, OpenTelemetry, trace_id в ошибках | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Бенчи: criterion + A/B-ворота в CI, `musubi bench/soak` | ✅ | ❌ | ❌ | ❌ | ❌ |
| SDK клиентов | 🟡 Unreal-скаффолд (`sdk/unreal`), остальные нет | Unreal/Unity | большинство | Unreal/Unity/JS | Unreal/Unity |

## 6. Истинный серверный мешинг

Что есть (`musubi-mesh`, `realtime/world.rs`): авторитет по ячейкам, handover с буферизацией
ввода и групповой передачей (транспорт + пассажиры), backoff после таймаута Ack, интерес
(grid + per-client), co-locate, interact через RL, lag-compensation (`rewind`), split/merge по cost,
soft-cap очередь, hysteresis, time dilation, размещение по регионам, fencing персистенции по term,
восстановление после падения хоста, provenance при сборке шардов, attach/detach в протоколе.

Чего нет для «мир сквозной, всё реплицируется между серверами»:

| Компонент | Статус | Что это значит |
|---|---|---|
| Replication layer как отдельный слой (граф сущностей хранит и стримит состояние, симуляторы stateless) | ❌ | состояние живёт в хосте шарда; смерть хоста = восстановление из снапшота+журнала, а не бесшовная замена симулятора |
| Кросс-граничные реплики (ghost/proxy сущностей соседних ячеек внутри каждого симулятора) | ✅ | хост ячейки каждый тик стримит приграничную полосу (`ghost_margin`) хостам соседей как read-only ghosts; в `view/{x}/{y}` они сливаются с owned; устаревание по тикам (`ghost_stale_ticks`) — `musubi-mesh/src/ghost.rs` |
| Репликация server↔server на уровне компонентов (дельты, приоритеты, bandwidth budget) | ✅ | ghost-поток — дельты по компонентам (`changed_rev`), приоритет по расстоянию до границы, бюджет байт на ячейку в тик (`ghost_budget_bytes`), keyframe раз в `ghost_keyframe_ticks`; handover строится на ghost'е принимающей ноды: уходят только компоненты новее её ревизии (`base_rev`) + буфер ввода, без ghost'а — `resend_full` и повтор полным состоянием |
| Единый entity id space + адресация через директорию для любого запроса из любой ячейки | ✅ | директория Raft |
| Персистентный entity streaming (мир живёт без игроков, выгрузка холодных ячеек) | ✅ | снапшоты/журнал; hibernate по отсутствию интереса, затем `unload_after_ticks` — персистентные сущности уходят в `world_cold_cells` и освобождают память, возвращаются при интересе/lease/спавне; `cold_cells` в инфо мира |
| Детерминизм и replay симуляции для отладки мешинга | 🟡 | replay матчей есть; replay мира с provenance — частично |
| Кросс-ячеечные транзакции (атомарный обмен предметами между сущностями разных серверов) | ✅ | `POST /v1/server/worlds/{w}/tx` — двухфазно по шине: prepare с проверкой версий компонентов и удержанием сущностей (запись 409, handover отложен), голоса хостов, commit/abort везде, таймаут по тикам, идемпотентно по tx id; локальный случай атомарно без шины |

## 7. Очередь работ

Порядок — по сочетанию «блокирует паритет с Pragma/AccelByte» × «нужно для истинного мешинга» ×
«нельзя сделать поверх модулем».

1. **Платформенные аккаунты консолей**: Xbox Live XSTS, PSN id_token, Nintendo NSA; импорт друзей с платформ.
2. **Entitlements**: Steam DLC/ownership, PSN, Xbox → предметы инвентаря, идемпотентный синк; IAP PSN/Xbox/Nintendo.
3. **Жалобы на игроков** общего вида с уликами (match id, метка реплея), очередь ревью, решение с баном из той же ручки.
4. **Limited grants** + per-key квоты и rate-limit.
5. **End-game report**: один маршрут «результаты матча → рейтинги + награды + статы + квесты», идемпотентный по матчу (замыкает game flow Pragma).
6. ~~**Мешинг I — кросс-граничные реплики**~~ ✅ — непрерывный поток ghost-дельт соседних ячеек с приоритетами, бюджетом и keyframe.
7. ~~**Мешинг II — handover без пересылки**~~ ✅ — handover передаёт только компоненты новее ревизии ghost'а на принимающей ноде.
8. **Мешинг III — replication layer**: вынос хранения/стрима состояния из хоста шарда в отдельный слой, stateless-симуляторы, бесшовная замена.
9. ~~**Мешинг IV — кросс-ячеечные транзакции и авто-выгрузка холодных ячеек**~~ ✅
10. **Окружения и версии контента/конфига**: ~~контент-бандлы с diff/историей/откатом~~ ✅; осталось: версии item definitions с миграцией инвентарей, конфиг кластера по окружениям.
11. ~~**RBAC операторов**~~ ✅ (роли ключей); мультипроект и SSO операторов — открыто.
12. ~~**Подписки IAP** (продление, grace)~~ ✅; WebPush, SMTP-провайдер — открыто.
13. **Схемы player data и миграции**, live-подписки на объекты, вторичные индексы.
14. **Брекеты турниров**, крафт, UGC.
15. **Античит-сигналы**: серверная валидация ввода в authoritative-матчах, интеграция EAC/BattlEye.
16. **Dev-инструменты**: фикстуры, запись/воспроизведение запросов, канареечный rollout модулей, Helm.
17. **Геороутинг**, scale-to-zero флотов, Edgegap/GameLift как провайдеры.

Клиентские SDK и GDK намеренно после: wire-протокол мира меняется пунктами 6–8, писать SDK до них —
переписывать дважды.
