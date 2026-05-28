# Usage Scenarios

## 1. Local Development — Proxy Mode

Ocular runs on: **Dev machine (192.168.1.100)**

```mermaid
graph LR
    subgraph Dev Machine 192.168.1.100
        App[App<br/>connects to 127.0.0.1:13306]
        Ocular[🟢 Ocular Proxy<br/>listens 127.0.0.1:13306]
    end
    subgraph Server 192.168.0.184
        MySQL[MySQL<br/>0.0.0.0:3306]
    end
    App --> Ocular
    Ocular --> MySQL
```

**CLI:**
```bash
ocular proxy mysql 192.168.0.184
# Listens on 127.0.0.1:13306, forwards to 192.168.0.184:3306
```

**Config (ocular.toml):**
```toml
[[proxy]]
name = "mysql-dev"
protocol = "mysql"
listen = "127.0.0.1:13306"
remote = "192.168.0.184:3306"
```

> ✅ Auto SSL stripping | Requires changing app connection to `127.0.0.1:13306`

---

## 2. Local Development — Capture Mode

Ocular runs on: **Dev machine (192.168.1.100)**

```mermaid
graph LR
    subgraph Dev Machine 192.168.1.100
        App[App<br/>connects to 192.168.0.184:6379]
        Ocular[🟢 Ocular Capture<br/>sniffs en0]
    end
    subgraph Server 192.168.0.184
        Redis[Redis<br/>0.0.0.0:6379]
    end
    App -->|en0 outbound| Redis
    Ocular -.->|passive capture on en0| App
```

**CLI:**
```bash
sudo ocular capture redis 192.168.0.184 -i en0
# Captures Redis traffic on en0 to 192.168.0.184:6379
```

**Config (ocular.toml):**
```toml
[[proxy]]
name = "redis-dev"
protocol = "redis"
mode = "capture"
interface = "en0"
remote = "192.168.0.184:6379"
```

> ❌ Cannot decrypt SSL traffic | Zero config on app side, requires sudo

---

## 3. Docker Compose — Proxy Mode

Ocular runs on: **Host machine (192.168.1.100)**

```mermaid
graph LR
    subgraph Host 192.168.1.100
        Ocular[🟢 Ocular Proxy<br/>listens 127.0.0.1:13306]
    end
    subgraph Docker Containers
        Client[Client container<br/>connects to host.docker.internal:13306]
        MySQL[MySQL container<br/>mapped to 127.0.0.1:3306]
    end
    Client --> Ocular
    Ocular --> MySQL
```

**CLI:**
```bash
ocular proxy mysql 127.0.0.1:3306 -l 127.0.0.1:13306
```

**Config (ocular.toml):**
```toml
[[proxy]]
name = "mysql"
protocol = "mysql"
listen = "127.0.0.1:13306"
remote = "127.0.0.1:3306"
```

**Docker client env:**
```yaml
environment:
  MYSQL_HOST: host.docker.internal
  MYSQL_PORT: 13306
```

> ✅ Auto SSL stripping | Docker service must map port to host (`ports: "3306:3306"`)

---

## 4. Docker Services — Capture Mode

Ocular runs on: **Host machine (192.168.1.100)**

```mermaid
graph LR
    subgraph Host 192.168.1.100
        CLI[redis-cli / mongosh<br/>connects to 127.0.0.1:6379]
        Ocular[🟢 Ocular Capture<br/>sniffs lo0]
    end
    subgraph Docker Containers
        Redis[Redis container<br/>mapped to 127.0.0.1:6379]
    end
    CLI -->|lo0| Redis
    Ocular -.->|passive capture on lo0| CLI
```

**CLI:**
```bash
sudo ocular capture redis 127.0.0.1 -i lo0    # macOS
sudo ocular capture redis 127.0.0.1 -i lo     # Linux
```

**Config (ocular.toml):**
```toml
[[proxy]]
name = "redis"
protocol = "redis"
mode = "capture"
interface = "lo0"            # macOS: lo0, Linux: lo
remote = "127.0.0.1:6379"
```

> Note: Traffic between containers (not routed through host NIC) is invisible to capture

```mermaid
graph LR
    subgraph Host 192.168.1.100
        Ocular[🟢 Ocular Capture<br/>sniffs lo0 ❌ cannot see]
    end
    subgraph Docker Internal Network 172.23.0.0/16
        Client[Client container<br/>172.23.0.5]
        MySQL[MySQL container<br/>172.23.0.9:3306]
    end
    Client -->|Docker bridge<br/>never reaches host NIC| MySQL
    Ocular -.->|❌ invisible| Client
```

> This traffic stays inside the Docker VM (macOS) or bridge network (Linux) and never passes through the host's lo0/eth0.

---

## 5. Production Server — Capture Mode

Ocular runs on: **Server (10.0.0.10)**

```mermaid
graph LR
    subgraph Clients
        A[App A<br/>10.0.0.5]
        B[App B<br/>10.0.0.8]
    end
    subgraph Server 10.0.0.10
        Redis[Redis<br/>0.0.0.0:6379]
        Ocular[🟢 Ocular Capture<br/>sniffs eth0]
    end
    A -->|10.0.0.10:6379| Redis
    B -->|10.0.0.10:6379| Redis
    Ocular -.->|passive capture on eth0| Redis
```

**CLI:**
```bash
sudo ocular capture redis 10.0.0.10:6379 -i eth0
# Or auto-detect interface:
sudo ocular capture redis 10.0.0.10
```

**Config (ocular.toml):**
```toml
[[proxy]]
name = "redis-prod"
protocol = "redis"
mode = "capture"
interface = "eth0"
remote = "10.0.0.10:6379"
```

**Permission (persistent, no sudo needed after this):**
```bash
sudo setcap cap_net_raw+ep $(which ocular)
```

> Shows all client IPs in `src` field | Zero intrusion | ❌ Cannot decrypt SSL traffic

---

## 6. Production Server — Proxy Mode (Sidecar)

Ocular runs on: **Server (10.0.0.10)**

```mermaid
graph LR
    subgraph Clients
        A[App A<br/>10.0.0.5]
        B[App B<br/>10.0.0.8]
    end
    subgraph Server 10.0.0.10
        Ocular[🟢 Ocular Proxy<br/>listens 0.0.0.0:13306]
        MySQL[MySQL<br/>127.0.0.1:3306]
    end
    A -->|10.0.0.10:13306| Ocular
    B -->|10.0.0.10:13306| Ocular
    Ocular --> MySQL
```

**CLI:**
```bash
ocular proxy mysql 127.0.0.1:3306 -l 0.0.0.0:13306
```

**Config (ocular.toml):**
```toml
[[proxy]]
name = "mysql-prod"
protocol = "mysql"
listen = "0.0.0.0:13306"
remote = "127.0.0.1:3306"
```

> ✅ Auto SSL stripping | Clients must connect to :13306 instead of :3306

---

## 7. CI Automation — CLI Mode

Ocular runs on: **CI Runner**

```mermaid
graph LR
    subgraph CI Runner
        Test[Test Script]
        Ocular[🟢 Ocular CLI<br/>proxy mysql --json<br/>listens 127.0.0.1:13306]
        Log[stdout → events.json]
    end
    subgraph Test Services
        MySQL[MySQL<br/>127.0.0.1:3306]
    end
    Test --> Ocular
    Ocular --> MySQL
    Ocular --> Log
```

**Commands:**
```bash
# Start proxy in background, output JSON
ocular proxy mysql --json > events.json &

# Run tests (connecting to 127.0.0.1:13306)
pytest --db-port=13306

# Analyze captured events
cat events.json | jq '.command'
```

---

## Decision Flowchart

```mermaid
flowchart TD
    Start[What traffic to observe?] --> Q1{Can you change app connection config?}
    Q1 -->|YES| Proxy[✅ Proxy Mode<br/>Any protocol, handles SSL]
    Q1 -->|NO| Q2{Is the protocol encrypted?}
    Q2 -->|NO<br/>Redis/Kafka/MongoDB<br/>AMQP/HTTP/Memcached| Capture[✅ Capture Mode<br/>Zero intrusion, just sudo]
    Q2 -->|YES<br/>MySQL/PG with SSL| Q3{Can you modify the server?}
    Q3 -->|Can disable SSL| CaptureSSL[Capture Mode<br/>disable SSL on server]
    Q3 -->|Can deploy sidecar| ProxySide[Proxy Mode<br/>deploy Ocular on server]
    Q3 -->|Neither| Native[Use native monitoring<br/>slow log, performance_schema]
```
