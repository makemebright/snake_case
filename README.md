# Taiga Snake

Бот для Battlesnake Arena на Rust. В дуэлях перебирает одновременные ходы обоих
игроков и углубляет поиск до исчерпания лимита по времени или числу позиций.
Оценка учитывает длину, доступную территорию, близость еды и свободу следующего
хода. При трёх и более живых змеях бот оценивает ближайшие ходы с учётом возможных
столкновений головами. На поле Royale бот учитывает текущие опасные клетки.
В Constrictor приоритет получает пространство для роста.

Стратегия написана для этого проекта. Симуляция подключает официальный
[движок Battlesnake Arena](https://github.com/BattlesnakeOfficial/arena), версия
зафиксирована в `Cargo.lock`. Поиск рассматривает текущую еду и опасные клетки.
Новая еда и сжатие поля воспроизводятся в локальных партиях движком.

## Запуск в WSL или Linux

Потребуются Rust с Cargo версии 1.90 или новее, Git и Python 3 для замера HTTP.

```bash
cargo build --release --locked --features lab --bins
PORT=18080 taskset -c 0 ./target/release/taiga-snake
```

Во втором терминале проверьте метаданные и все четыре HTTP-обработчика.
Сервер отвечает на `GET /`, `POST /start`, `POST /move` и `POST /end`.

```bash
curl -fsS http://127.0.0.1:18080/
./target/release/lab samples --mode duels --seed 21 --count 64 \
  | python3 scripts/measure_http.py http://127.0.0.1:18080
```

Если тест запускается с машины внутри того же tailnet, публичный Funnel-домен
может резолвиться в локальный Tailscale IP. Флаг `--connect-ip` позволяет
замерить именно внешний маршрут, сохраняя правильные TLS SNI и Host:

```bash
python3 scripts/measure_http.py https://snake.example.ts.net \
  --connect-ip 203.0.113.10 --repeats 10
```

Лимит поиска на один ход составляет до 32 мс при обычном тайм-ауте игры 500 мс,
до 6 000 позиций и до восьми ходов вперёд. Поиск возвращает результат последней
завершённой глубины. Tokio запускает один поток для HTTP и вычисления ходов.
При публикации замерьте также задержку от движка Arena до сервера в профиле змеи.

Можно запустить сервер из контейнера с лимитом в одно ядро.

```bash
docker build -t taiga-snake .
docker run --rm --cpus=1 -p 127.0.0.1:18080:8080 taiga-snake
```

## Локальные партии и подбор параметров

`lab bench` запускает серии на официальном движке. Для каждого seed кандидат
играет с каждого стартового места. Доступны режимы `duels`, `standard`,
`royale` и `constrictor`, а также соперники `greedy`, `food`, `space`, `search`,
`search-food`, `search-space`, `search-shallow` и `search-deep`. Последние четыре
варианта позволяют проверять кандидата против поисков с другими весами и бюджетами.
Для воспроизводимости поиск в `lab` ограничен числом узлов и глубиной. Серия
печатает победы, ничьи, поражения, перцентили времени и достигнутой глубины.

```bash
taskset -c 0 ./target/release/lab bench --mode duels --opponent greedy \
  --first-seed 101 --seeds 12 --nodes 6000 --depth 8

taskset -c 0 ./target/release/lab bench --mode duels --opponent search-deep \
  --first-seed 101 --seeds 12 --nodes 6000 --depth 8 \
  --deaths experiments/search-deep.json
```

Флаг `--deaths` сохраняет JSON с seed, местом старта, причиной выбывания,
последним решением и официальным `/move`-состоянием непосредственно перед смертью.
Журнал проведённых A/B-проверок находится в `experiments/README.md`.

`lab tune` варьирует веса длины, территории, еды и свободы хода. Каждую версию
он оценивает по разности числа побед и поражений на общей серии. В пул входят
стратегии с приоритетом еды и пространства, а также поисковая версия с исходными
весами. Веса из эксперимента записываются, если кандидат улучшил результат на
отдельных seed. Перед публикацией проверяйте найденные веса на более длинной
серии с лимитами поиска сервера.

```bash
taskset -c 0 ./target/release/lab tune --mode duels --first-seed 1201 \
  --seeds 12 --nodes 600 --rounds 2 --out weights.json

taskset -c 0 ./target/release/lab bench --mode duels --opponent search \
  --first-seed 10001 --seeds 24 --nodes 6000 --depth 8 --weights weights.json
```

Переменная среды `SNAKE_WEIGHTS` принимает содержимое `weights.json`. С ней можно
проверить новую конфигурацию локально и затем использовать те же веса на хостинге.

```bash
SNAKE_WEIGHTS="$(cat weights.json)" PORT=18080 ./target/release/taiga-snake
```

## Публикация

Для временного хостинга с Windows можно оставить Docker-контейнер работающим и
открыть локальный порт через Tailscale Funnel:

```powershell
tailscale funnel --bg http://127.0.0.1:18080
tailscale funnel status
```

Funnel и Tailscale должны оставаться запущенными, а компьютер — подключённым к
интернету и не переходить в сон. Для постоянного хостинга используйте Railway:

1. Создайте постоянно работающий сервис Railway из этого проекта. Railway
   увидит `Dockerfile`. Сервер слушает `0.0.0.0:$PORT`, значение по умолчанию
   в контейнере 8080. Для проверки готовности укажите путь `/`.
   [Документация Railway по Dockerfile](https://docs.railway.com/builds/dockerfiles).
2. В настройках сервиса откройте **Networking → Public Networking → Generate Domain**.
   Оставьте Serverless выключенным. Проверьте ответ `GET /` по публичному HTTPS.
   [Домены Railway](https://docs.railway.com/networking/domains/working-with-domains).
3. Войдите на Arena через GitHub, откройте **My Snakes → Add New Battlesnake**,
   укажите корневой URL сервиса и регион движка рядом с хостингом. Откройте
   **Duels 11x11**, выберите змею в **Your Snakes** и нажмите **Join**. Для общего
   рейтинга повторите регистрацию этой змеи в Standard, Royale и Constrictor.
   [Таблицы Arena](https://arena.battlesnake.com/leaderboards).
4. После первых партий проверьте p95, тайм-ауты и положение в Duels. Обновляйте
   сервис по тому же URL, чтобы продолжать играть с той же регистрацией.

Для резервной сдачи опубликуйте этот проект в открытом GitHub-репозитории.
Команда курса может запустить сервер по этой инструкции и передать его адрес
в официальный [симулятор Battlesnake](https://github.com/BattlesnakeOfficial/rules/tree/main/cli).
Победы над локальными тестовыми стратегиями показывают результат эксперимента.
Зачёт определяют позиции на Arena или дуэли команды курса с эталонами.
