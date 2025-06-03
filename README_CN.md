# heart-rate-proxy

本项目是一个玩具!用于将Apple Watch心率数据转发到 GreptimeDB 数据库。该服务基于 `axum` 框架，提供 HTTP 接口，支持异步处理和高性能数据传输。

## 依赖

- Rust 2021 Edition
- axum 0.7
- tokio 1.0
- tower & tower-http
- serde
- reqwest
- chrono
- regex

## 快速开始

1. **克隆仓库**
   ```bash
   git clone https://github.com/zqr10159/heartbeat-greptime.git
   cd heartbeat-greptime
   ```

2. **构建并运行**
   ```bash
   cargo run
   ```

3. **配置与使用**
    - 手机端配置
        - 需配置快捷指令: [iCloud Shortcut](https://www.icloud.com/shortcuts/2dc5d3614f204dd6af396d04c773bfbf), 根据自己的服务器地址修改 URL。
        - 在快捷指令-自动化中添加触发条件，如每天或每小时添加一个任务,确保心率数据能自动发送到服务器。
    - 服务器端配置
        - 根据实际情况修改环境变量`GREPTIME_URL`和`GREPTIME_DB`, 如数据库需要鉴权, 自行修改代码添加鉴权请求头, 参考`https://docs.greptime.cn/user-guide/ingest-data/for-iot/influxdb-line-protocol`

4. **效果截图**
    - 通过浏览器访问`GreptimeDB dashboard` 查看心率数据图。![img.png](img.png)
## 许可证

本项目遵循 Apache 2.0 许可证，详见 [LICENSE](./LICENSE)。

---