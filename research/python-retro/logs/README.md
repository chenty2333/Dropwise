# 日志与数字的对应关系

每个文件是 `cases/*.py` 的一次完整运行（stdout+stderr）。最终数字从这些
文件里的 `SUMMARY` / `VIOLATION` / `instrument=` / JSON 行直接得到。

| 文件模式 | 含义 |
|---|---|
| `redis-blpop-<版本>-r<轮>-<层级>.log` | R1：BLPOP，4.5.3/4.5.4，B1/B2/B3/M1，3 轮 |
| `redis-pipeline-<版本>-r<轮>-<层级>.log` | R2：pipeline，4.5.2/4.5.3，B1/B2/B3/M1，3 轮 |
| `sqla-timeout-begin-<版本>-r<轮>-<层级>.log` | SQLAlchemy timeout-BEGIN 注入，1.4.17/1.4.18 |
| `sqla-ladder-<版本>-r<轮>-<层级>.log` | SQLAlchemy 纯取消阶梯，计划 2–6 |
| `aiohttp-<版本>-r<轮>-<层级>.log` | aiohttp 3.14.0/3.14.1，四个层级，含插桩行 |
| `aiohttp-<版本>-r<轮>-idle.log` | idle `sock_read` 诊断，不计入层级 |
| `run-*-console.txt` | runner 的控制台输出（容器、层级顺序、退出码） |
| `docker-ps-*.txt` | 运行结束时的 `docker ps -a` |
| `environment.txt` | 镜像 digest、服务端版本、Python/包版本、`uv pip freeze` |

每个日志的 `case=` 行给出被测库版本和 round；`SUMMARY` 行给出 K、trials、
violations、unknown、unrealized；M1 的 `pre=`/`post=` 和逐计划 `obs=` 是
判定依据。`PRIVATE-NOTES.md` 的 P1/P2 来自 `sqla-ladder-*` 的 #2/#7 观察。
