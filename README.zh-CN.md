# <img src="assets/icon/dbdelve.svg" alt="" width="48" align="absmiddle"> DBDelve

[English](README.md) · **简体中文**

> 本文是 [README.md](README.md) 的简体中文翻译，与原文同受 GPL-3.0 约束。若有出入，以英文原文为准。

快速、原生、跨平台的多引擎数据库客户端。用 Rust 和 GPUI 写成，没有 Electron 也没有 JVM：启动即完成，内存占用友好，在大表上滚动依旧顺滑——别的客户端在这些场景里不是崩溃，就是给你加上一堆限制。

> **这是一个 fork。** 它由 [nutlet85/dbdelve](https://github.com/nutlet85/dbdelve) 维护，并自行发布 Windows 构建。DBDelve 本身属于
> [ShayanAbbas1/dbdelve](https://github.com/ShayanAbbas1/dbdelve)，作者是 Shayan Abbas：macOS 与 Linux 构建、Homebrew cask、AUR 包、Discord 以及项目的筹款都归他，不属于这个 fork。
> 这里的代码和上游一样，是 GPL-3.0-only。

## 社区

关于 DBDelve 本身，请去上游的社区提问：[Discord](https://discord.gg/7PegWzwqCm)，以及[上游仓库](https://github.com/ShayanAbbas1/dbdelve)的 issue 与讨论。这个 fork 自己的改动记录在它的 [issues](https://github.com/nutlet85/dbdelve/issues) 里。

![Gruvbox Dark Glass 主题下的 DBDelve：编辑器里对 events 表的 SELECT，下方是结果网格](assets/screenshots/query-dark.png)

![Gruvbox Light Glass 主题下的 DBDelve：编辑器里的一个 join，下方是结果网格](assets/screenshots/query-light.png)

滚动一百万行：

https://github.com/user-attachments/assets/6e15d566-a110-46c6-a236-52af7d877d4c

启动，上次的结果还在：

https://github.com/user-attachments/assets/9e55da56-8577-43e0-812a-c77eb5d0e701

## 为什么

我想要一个快、现代、又省内存的东西。现有的数据库客户端要么臃肿（Electron 和 JVM），要么收费。这就是它们的替代品。不会有任何功能被放进付费墙，随着时间推移，功能上也会与它们完全对齐。

## 安装

**Windows**（x86_64）是这个 fork 构建的版本：从[此 fork 的最新 release][releases] 运行安装程序，或解压 zip 后运行 `dbdelve.exe`。安装程序会让你选择装进 AppData（仅当前用户）还是 Program Files（所有用户）。应用从这个仓库的 release 自动更新，更新包用此 fork 自己的密钥签名。

**macOS 和 Linux** 不在这里构建。请从[上游的 release](https://github.com/ShayanAbbas1/dbdelve/releases/latest) 安装，并注意上游构建是从上游自动更新的。

从源码构建、你的数据存放在哪里，以及其他内容，见 [docs/install.md](docs/install.md)。

[releases]: https://github.com/nutlet85/dbdelve/releases/latest

## 支持的数据库

Postgres、MySQL、MariaDB、SQLite、Turso、DuckDB、Snowflake、SQL Server、MongoDB、Redis（以及 Valkey、KeyDB、Dragonfly）和 ClickHouse。

任何有 ODBC 驱动的数据库都可以通过内置的 ODBC 桥接访问，前提是装上那个驱动：IBM Db2、Oracle、SAP HANA、SAP ASE（Sybase）、Informix、Teradata、Vertica、Exasol、Firebird、Amazon Redshift、Google BigQuery、Databricks、Apache Hive、Trino 和 Presto、Dremio、InterSystems IRIS、Microsoft Access 等等。这时网格编辑会关闭，你输入的语句按连接的权限模式执行。见[通过 ODBC 连接](docs/odbc.md)。

![欢迎界面：粘贴连接 URL，或者挑一个引擎，从 DBeaver 或 TablePlus 导入，或者选一个主题](assets/screenshots/welcome.png)

## 它能做什么

- **让它变成你的。** 应用自带主题，另有 Catppuccin、Tokyo Night、Gruvbox 和 Dracula，每个都有暗色、亮色和玻璃三种。可以从 Zed 或 VS Code 导入主题，也可以[自己写一个](docs/themes.md)。

  ![主题选择器：从 Zed 和 VS Code 导入的主题，下方是导入命令](assets/screenshots/themes.png)

- **把连接一起带过来。** 从 DBeaver（macOS 上还有 TablePlus）导入所有已保存的连接，包括密码和 SSH 设置。
- **把连接组织成项目**，从命令面板跳到任意一个，或者断开连接回到列出全部连接的主界面。

  ![按连接筛选的命令面板，连接分成两个项目，当前连接有标记](assets/screenshots/projects.png)

- **永远不会把生产当成预发。** 每个连接彼此隔离，并且可以给标题栏上色。可以让它只读、可读写或完全访问运行，任何破坏性操作之前 DBDelve 都会先问。

  ![连接表单，按连接、安全和行为分组，可以为连接指定颜色和项目](assets/screenshots/connection-form.png)

- **先看懂，再修改。** 改一个单元格，DBDelve 会替你写出 `UPDATE`，运行前可以先检查。
- **留下你需要的列。** 隐藏或固定列，布局重启后依然保留。
- **完整读一行**：行面板里高亮显示 JSON，长值也可以在那里编辑。

  ![表格在网格中打开，行面板显示一行里的值，并高亮其中的 JSON](assets/screenshots/table-dark.png)

- **导入行**：支持 CSV、TSV 和 JSON，每一行都会校验，每条语句都会在任何东西运行之前展示出来。

  ![导入 events.csv：左侧是格式选项，每个文件列都映射到表列，下方是行的预览](assets/screenshots/import.png)

- **打开数据文件**：把它拖到窗口上即可。CSV、Excel、Parquet 或 JSON 文件会在内存里的 DuckDB 中打开，成为一个可以改的查询。`.sql` 文件会在当前连接上的新查询标签页里打开，只有在你运行时才执行。

- **查看查询计划**：在独立面板里高亮显示，糟糕的行数估算会被标出来。

  ![一个 join 的 Explain Analyze 计划：每个节点都有实际与估算行数和耗时，其中 ×76 的误估算被标记](assets/screenshots/query-plan.png)

- **懂你 schema 的补全**：表名，以及表名加一个点之后的列。表会自带别名写入，`JOIN` 之后还会自动补上外键隐含的条件。

  ![在 join 中把 accounts. 补全为它的列](assets/screenshots/completion.png)

- **键盘优先。** 一个命令面板管命令、表、已保存的查询、历史和连接，每个快捷键都可以改。

  <img src="assets/screenshots/shortcuts.png" width="360" alt="设置里的快捷键页面：每个操作都列出按键绑定和编辑按钮">

- **用的是你自己的 SSH，不是重新实现一套。** 隧道跑在你自己的 `ssh` 上，所以你的配置、agent 和硬件密钥直接可用。
  对 Postgres 来说，Host 写成目录（`/var/run/postgresql`）就会连到 SSH 主机上的那个 Unix socket，还可以切换到别的用户（Run as）；服务器上需要 `socat`，而设置了 Run as 时还需要免密码的 `sudo`。`disable` 和 `prefer` 之外的 TLS 模式在这里会被拒绝。
- **用短期令牌登录。** 在 Postgres、MySQL、MariaDB、SQL Server、Redis 和 ClickHouse 上，profile 的 Password 命令会在每次登录时打印 password，所以云 IAM auth 在令牌过期后照样可用。以 Amazon RDS 为例：

  ```sh
  /opt/homebrew/bin/aws rds generate-db-auth-token --hostname %HOSTNAME% --port %PORT% --username %USERNAME% --region <region>
  ```

  `%HOSTNAME%`、`%PORT%` 和 `%USERNAME%` 取自 profile 自己。DBDelve 会自行给它们加引号，所以不要自己加：写在引号里会让命令无法执行。请填写程序的完整路径（`which aws` 会打印出来）：从 Finder 或桌面启动器打开的应用看不到你 shell 设置的 PATH。「Expires after」会在这段时间内复用命令的输出，且只留在内存中；留空则每次登录都执行一遍命令。

自由开源，没有付费墙，也没有遥测：DBDelve 自己唯一会发出的请求是检查更新，而你可以关掉它。

## 基准测试

DBDelve 0.3.6 对比 DBeaver 25.3.1，同一张 Postgres 表，各取 5 次运行的中位数（2026-10-11，M1 Pro，32 GB）：

|                    | DBDelve | DBeaver |
| ------------------ | ------- | ------- |
| 启动               | 1.1 s   | 5.2 s   |
| 空闲内存           | 133 MB  | 606 MB  |
| 1000 行查询        | 0.21 s  | 1.96 s  |
| 100 万行，耗时     | 56 s    | 208 s   |
| 100 万行，内存\*   | 4.9 GB  | 17 GB   |

\* 有意把整张 100 万行的表拉进内存，并关掉了行数上限。方法与完整结果见 [BENCHMARKS.md](BENCHMARKS.md)。

## 日志与缺陷报告

DBDelve 会在你的机器上保留一份诊断日志，从不发往任何地方。要把它附到 issue 里，用 **Help → Copy Diagnostics** 复制粘贴；**Help → Open Logs Folder** 会显示这些文件。如果默认日志看不出问题，在设置里打开 **Verbose logging** 再复现一次。

发出来之前先读一遍日志，删掉任何敏感内容。详细的日志可能包含主机名、数据库名，以及数据库驱动记录下的任何东西。

## 许可

Copyright (C) 2026 Shayan Abbas and contributors，以 [GPL-3.0](LICENSE) 授权。切换到该许可之前贡献的代码以 [MIT License](licenses/MIT-dbdelve.txt) 发布，那份声明对它仍然适用。第三方字体与主题的许可见 [NOTICES.md](NOTICES.md)。
