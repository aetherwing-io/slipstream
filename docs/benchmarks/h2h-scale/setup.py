#!/usr/bin/env python3
"""Generate and verify 20-file scale test for H2H benchmark."""

import os
import sys

FILES = {
    "auth_service.py": '''\
from dataclasses import dataclass
from typing import Optional
from db_pool import get_connection


@dataclass
class AuthToken:
    user_id: str
    token: str
    expires_at: float


class AuthService:
    """Handles authentication and token management."""

    def __init__(self, secret_key: str):
        self.secret_key = secret_key

    def authenticate(self, username: str, password: str) -> Optional[AuthToken]:
        """Validate credentials and return a token."""
        conn = get_connection()
        row = conn.execute(
            "SELECT id, password_hash FROM users WHERE username = ?",
            (username,),
        ).fetchone()
        if row and self._verify_password(password, row["password_hash"]):
            return self._create_token(row["id"])
        return None

    def _verify_password(self, password: str, hashed: str) -> bool:
        import hashlib
        return hashlib.sha256(password.encode()).hexdigest() == hashed

    def _create_token(self, user_id: str) -> AuthToken:
        import time
        import secrets
        return AuthToken(
            user_id=user_id,
            token=secrets.token_urlsafe(32),
            expires_at=time.time() + 3600,
        )

    def revoke_token(self, token: str) -> bool:
        conn = get_connection()
        conn.execute("DELETE FROM tokens WHERE token = ?", (token,))
        return True
''',
    "user_service.py": '''\
from dataclasses import dataclass, field
from typing import List, Optional
from db_pool import get_connection


@dataclass
class User:
    id: str
    username: str
    email: str
    roles: List[str] = field(default_factory=list)


class UserService:
    """Manages user accounts and profiles."""

    def get_user(self, user_id: str) -> Optional[User]:
        conn = get_connection()
        row = conn.execute("SELECT * FROM users WHERE id = ?", (user_id,)).fetchone()
        if row:
            return User(id=row["id"], username=row["username"], email=row["email"])
        return None

    def create_user(self, username: str, email: str) -> User:
        import uuid
        user_id = str(uuid.uuid4())
        conn = get_connection()
        conn.execute(
            "INSERT INTO users (id, username, email) VALUES (?, ?, ?)",
            (user_id, username, email),
        )
        return User(id=user_id, username=username, email=email)

    def list_users(self, limit: int = 100) -> List[User]:
        conn = get_connection()
        rows = conn.execute("SELECT * FROM users LIMIT ?", (limit,)).fetchall()
        return [User(id=r["id"], username=r["username"], email=r["email"]) for r in rows]

    def delete_user(self, user_id: str) -> bool:
        conn = get_connection()
        conn.execute("DELETE FROM users WHERE id = ?", (user_id,))
        return True
''',
    "order_service.py": '''\
from dataclasses import dataclass, field
from typing import List
from enum import Enum
from db_pool import get_connection


class OrderStatus(Enum):
    PENDING = "pending"
    CONFIRMED = "confirmed"
    SHIPPED = "shipped"
    DELIVERED = "delivered"
    CANCELLED = "cancelled"


@dataclass
class OrderItem:
    product_id: str
    quantity: int
    unit_price: float


@dataclass
class Order:
    id: str
    user_id: str
    status: OrderStatus
    items: List[OrderItem] = field(default_factory=list)

    @property
    def total(self) -> float:
        return sum(i.quantity * i.unit_price for i in self.items)


class OrderService:
    """Manages order lifecycle."""

    def create_order(self, user_id: str, items: List[OrderItem]) -> Order:
        import uuid
        order = Order(id=str(uuid.uuid4()), user_id=user_id, status=OrderStatus.PENDING, items=items)
        conn = get_connection()
        conn.execute(
            "INSERT INTO orders (id, user_id, status) VALUES (?, ?, ?)",
            (order.id, user_id, order.status.value),
        )
        return order

    def cancel_order(self, order_id: str) -> bool:
        conn = get_connection()
        conn.execute(
            "UPDATE orders SET status = ? WHERE id = ?",
            (OrderStatus.CANCELLED.value, order_id),
        )
        return True

    def get_orders_for_user(self, user_id: str) -> List[Order]:
        conn = get_connection()
        rows = conn.execute("SELECT * FROM orders WHERE user_id = ?", (user_id,)).fetchall()
        return [Order(id=r["id"], user_id=r["user_id"], status=OrderStatus(r["status"])) for r in rows]
''',
    "payment_service.py": '''\
from dataclasses import dataclass
from typing import Optional
from enum import Enum
from db_pool import get_connection


class PaymentStatus(Enum):
    PENDING = "pending"
    COMPLETED = "completed"
    FAILED = "failed"
    REFUNDED = "refunded"


@dataclass
class Payment:
    id: str
    order_id: str
    amount: float
    status: PaymentStatus
    provider: str


class PaymentService:
    """Processes payments through configurable providers."""

    def __init__(self, default_provider: str = "stripe"):
        self.default_provider = default_provider

    def charge(self, order_id: str, amount: float) -> Payment:
        import uuid
        payment = Payment(
            id=str(uuid.uuid4()),
            order_id=order_id,
            amount=amount,
            status=PaymentStatus.PENDING,
            provider=self.default_provider,
        )
        conn = get_connection()
        conn.execute(
            "INSERT INTO payments (id, order_id, amount, status) VALUES (?, ?, ?, ?)",
            (payment.id, order_id, amount, payment.status.value),
        )
        return self._process_with_provider(payment)

    def _process_with_provider(self, payment: Payment) -> Payment:
        payment.status = PaymentStatus.COMPLETED
        return payment

    def refund(self, payment_id: str) -> Optional[Payment]:
        conn = get_connection()
        conn.execute(
            "UPDATE payments SET status = ? WHERE id = ?",
            (PaymentStatus.REFUNDED.value, payment_id),
        )
        return None
''',
    "inventory_service.py": '''\
from dataclasses import dataclass
from typing import Dict, Optional
from db_pool import get_connection


@dataclass
class StockLevel:
    product_id: str
    available: int
    reserved: int

    @property
    def total(self) -> int:
        return self.available + self.reserved


class InventoryService:
    """Tracks product stock levels and reservations."""

    def check_stock(self, product_id: str) -> Optional[StockLevel]:
        conn = get_connection()
        row = conn.execute(
            "SELECT * FROM inventory WHERE product_id = ?", (product_id,)
        ).fetchone()
        if row:
            return StockLevel(
                product_id=row["product_id"],
                available=row["available"],
                reserved=row["reserved"],
            )
        return None

    def reserve(self, product_id: str, quantity: int) -> bool:
        stock = self.check_stock(product_id)
        if stock and stock.available >= quantity:
            conn = get_connection()
            conn.execute(
                "UPDATE inventory SET available = available - ?, reserved = reserved + ? WHERE product_id = ?",
                (quantity, quantity, product_id),
            )
            return True
        return False

    def bulk_check(self, product_ids: list) -> Dict[str, StockLevel]:
        result = {}
        for pid in product_ids:
            level = self.check_stock(pid)
            if level:
                result[pid] = level
        return result
''',
    "shipping_service.py": '''\
from dataclasses import dataclass
from typing import Optional
from enum import Enum
from db_pool import get_connection


class ShipmentStatus(Enum):
    PREPARING = "preparing"
    IN_TRANSIT = "in_transit"
    DELIVERED = "delivered"


@dataclass
class Shipment:
    id: str
    order_id: str
    tracking_number: str
    status: ShipmentStatus
    carrier: str


class ShippingService:
    """Manages order shipments and tracking."""

    def __init__(self, default_carrier: str = "ups"):
        self.default_carrier = default_carrier

    def create_shipment(self, order_id: str) -> Shipment:
        import uuid
        shipment = Shipment(
            id=str(uuid.uuid4()),
            order_id=order_id,
            tracking_number=self._generate_tracking(),
            status=ShipmentStatus.PREPARING,
            carrier=self.default_carrier,
        )
        conn = get_connection()
        conn.execute(
            "INSERT INTO shipments (id, order_id, tracking) VALUES (?, ?, ?)",
            (shipment.id, order_id, shipment.tracking_number),
        )
        return shipment

    def _generate_tracking(self) -> str:
        import secrets
        return f"TRK-{secrets.token_hex(8).upper()}"

    def get_tracking(self, order_id: str) -> Optional[Shipment]:
        conn = get_connection()
        row = conn.execute("SELECT * FROM shipments WHERE order_id = ?", (order_id,)).fetchone()
        if row:
            return Shipment(
                id=row["id"], order_id=row["order_id"],
                tracking_number=row["tracking"], status=ShipmentStatus.PREPARING,
                carrier=self.default_carrier,
            )
        return None
''',
    "notification_service.py": '''\
from dataclasses import dataclass
from typing import List, Dict, Any
from enum import Enum


class Channel(Enum):
    EMAIL = "email"
    SMS = "sms"
    PUSH = "push"
    WEBHOOK = "webhook"


@dataclass
class Notification:
    recipient: str
    channel: Channel
    subject: str
    body: str
    metadata: Dict[str, Any] = None


class NotificationService:
    """Sends notifications across multiple channels."""

    def __init__(self):
        self._handlers = {
            Channel.EMAIL: self._send_email,
            Channel.SMS: self._send_sms,
            Channel.PUSH: self._send_push,
            Channel.WEBHOOK: self._send_webhook,
        }

    def send(self, notification: Notification) -> bool:
        handler = self._handlers.get(notification.channel)
        if handler:
            return handler(notification)
        return False

    def broadcast(self, recipients: List[str], channel: Channel, subject: str, body: str) -> int:
        sent = 0
        for recipient in recipients:
            n = Notification(recipient=recipient, channel=channel, subject=subject, body=body)
            if self.send(n):
                sent += 1
        return sent

    def _send_email(self, n: Notification) -> bool:
        return True

    def _send_sms(self, n: Notification) -> bool:
        return True

    def _send_push(self, n: Notification) -> bool:
        return True

    def _send_webhook(self, n: Notification) -> bool:
        return True
''',
    "search_service.py": '''\
from dataclasses import dataclass
from typing import List, Optional, Dict, Any


@dataclass
class SearchResult:
    id: str
    score: float
    source: str
    highlights: List[str]


class SearchService:
    """Full-text search across indexed entities."""

    def __init__(self, index_path: str = "/var/data/search"):
        self.index_path = index_path
        self._cache: Dict[str, List[SearchResult]] = {}

    def search(self, query: str, limit: int = 20, offset: int = 0) -> List[SearchResult]:
        cache_key = f"{query}:{limit}:{offset}"
        if cache_key in self._cache:
            return self._cache[cache_key]
        results = self._execute_search(query, limit, offset)
        self._cache[cache_key] = results
        return results

    def _execute_search(self, query: str, limit: int, offset: int) -> List[SearchResult]:
        return []

    def suggest(self, prefix: str, limit: int = 5) -> List[str]:
        return [f"{prefix}_suggestion_{i}" for i in range(limit)]

    def reindex(self, entity_type: str) -> int:
        return 0

    def clear_cache(self) -> None:
        self._cache.clear()
''',
    "analytics_service.py": '''\
from dataclasses import dataclass
from typing import Dict, Any, List, Optional
from datetime import datetime
from db_pool import get_connection


@dataclass
class Event:
    name: str
    user_id: str
    timestamp: datetime
    properties: Dict[str, Any]


class AnalyticsService:
    """Tracks and queries analytics events."""

    def track(self, event: Event) -> bool:
        conn = get_connection()
        conn.execute(
            "INSERT INTO events (name, user_id, timestamp, properties) VALUES (?, ?, ?, ?)",
            (event.name, event.user_id, event.timestamp.isoformat(), str(event.properties)),
        )
        return True

    def query(self, event_name: str, start: Optional[datetime] = None, end: Optional[datetime] = None) -> List[Event]:
        conn = get_connection()
        sql = "SELECT * FROM events WHERE name = ?"
        params: list = [event_name]
        if start:
            sql += " AND timestamp >= ?"
            params.append(start.isoformat())
        if end:
            sql += " AND timestamp <= ?"
            params.append(end.isoformat())
        rows = conn.execute(sql, params).fetchall()
        return [
            Event(name=r["name"], user_id=r["user_id"], timestamp=datetime.fromisoformat(r["timestamp"]), properties={})
            for r in rows
        ]

    def count(self, event_name: str) -> int:
        conn = get_connection()
        row = conn.execute("SELECT COUNT(*) as cnt FROM events WHERE name = ?", (event_name,)).fetchone()
        return row["cnt"] if row else 0
''',
    "cache_service.py": '''\
from dataclasses import dataclass
from typing import Any, Optional, Dict
import time


@dataclass
class CacheEntry:
    key: str
    value: Any
    expires_at: float
    created_at: float


class CacheService:
    """In-memory cache with TTL support."""

    def __init__(self, default_ttl: int = 300):
        self.default_ttl = default_ttl
        self._store: Dict[str, CacheEntry] = {}

    def get(self, key: str) -> Optional[Any]:
        entry = self._store.get(key)
        if entry and entry.expires_at > time.time():
            return entry.value
        if entry:
            del self._store[key]
        return None

    def set(self, key: str, value: Any, ttl: Optional[int] = None) -> None:
        now = time.time()
        self._store[key] = CacheEntry(
            key=key,
            value=value,
            expires_at=now + (ttl or self.default_ttl),
            created_at=now,
        )

    def delete(self, key: str) -> bool:
        if key in self._store:
            del self._store[key]
            return True
        return False

    def clear(self) -> None:
        self._store.clear()

    def stats(self) -> Dict[str, int]:
        now = time.time()
        total = len(self._store)
        expired = sum(1 for e in self._store.values() if e.expires_at <= now)
        return {"total": total, "active": total - expired, "expired": expired}
''',
    "rate_limiter.py": '''\
from dataclasses import dataclass, field
from typing import Dict, Optional
import time


@dataclass
class RateBucket:
    tokens: float
    last_refill: float
    max_tokens: float
    refill_rate: float


class RateLimiter:
    """Token bucket rate limiter."""

    def __init__(self, max_requests: int = 100, window_seconds: int = 60):
        self.max_requests = max_requests
        self.window_seconds = window_seconds
        self._buckets: Dict[str, RateBucket] = {}

    def allow(self, key: str) -> bool:
        bucket = self._get_or_create(key)
        self._refill(bucket)
        if bucket.tokens >= 1.0:
            bucket.tokens -= 1.0
            return True
        return False

    def remaining(self, key: str) -> int:
        bucket = self._buckets.get(key)
        if not bucket:
            return self.max_requests
        self._refill(bucket)
        return int(bucket.tokens)

    def _get_or_create(self, key: str) -> RateBucket:
        if key not in self._buckets:
            self._buckets[key] = RateBucket(
                tokens=float(self.max_requests),
                last_refill=time.time(),
                max_tokens=float(self.max_requests),
                refill_rate=self.max_requests / self.window_seconds,
            )
        return self._buckets[key]

    def _refill(self, bucket: RateBucket) -> None:
        now = time.time()
        elapsed = now - bucket.last_refill
        bucket.tokens = min(bucket.max_tokens, bucket.tokens + elapsed * bucket.refill_rate)
        bucket.last_refill = now

    def reset(self, key: str) -> None:
        if key in self._buckets:
            del self._buckets[key]
''',
    "health_check.py": '''\
from dataclasses import dataclass
from typing import Dict, List
from enum import Enum
from db_pool import get_connection


class HealthStatus(Enum):
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    UNHEALTHY = "unhealthy"


@dataclass
class ComponentHealth:
    name: str
    status: HealthStatus
    latency_ms: float
    message: str = ""


class HealthChecker:
    """Monitors health of system components."""

    def __init__(self):
        self._checks: List[callable] = []

    def register_check(self, check_fn: callable) -> None:
        self._checks.append(check_fn)

    def check_all(self) -> Dict[str, ComponentHealth]:
        results = {}
        for check in self._checks:
            health = check()
            results[health.name] = health
        return results

    def check_database(self) -> ComponentHealth:
        import time
        start = time.time()
        try:
            conn = get_connection()
            conn.execute("SELECT 1")
            latency = (time.time() - start) * 1000
            return ComponentHealth(name="database", status=HealthStatus.HEALTHY, latency_ms=latency)
        except Exception as e:
            return ComponentHealth(name="database", status=HealthStatus.UNHEALTHY, latency_ms=0, message=str(e))

    def overall_status(self) -> HealthStatus:
        results = self.check_all()
        if any(h.status == HealthStatus.UNHEALTHY for h in results.values()):
            return HealthStatus.UNHEALTHY
        if any(h.status == HealthStatus.DEGRADED for h in results.values()):
            return HealthStatus.DEGRADED
        return HealthStatus.HEALTHY
''',
    "middleware.py": '''\
from dataclasses import dataclass
from typing import Callable, Any, Dict, List


@dataclass
class Request:
    method: str
    path: str
    headers: Dict[str, str]
    body: Any = None


@dataclass
class Response:
    status: int
    headers: Dict[str, str]
    body: Any = None


class MiddlewareChain:
    """Composable middleware pipeline."""

    def __init__(self):
        self._middlewares: List[Callable] = []

    def use(self, middleware: Callable) -> "MiddlewareChain":
        self._middlewares.append(middleware)
        return self

    def execute(self, request: Request) -> Response:
        def build_chain(index: int) -> Callable:
            if index >= len(self._middlewares):
                return lambda req: Response(status=404, headers={}, body="Not Found")
            middleware = self._middlewares[index]
            next_fn = build_chain(index + 1)
            return lambda req: middleware(req, next_fn)

        chain = build_chain(0)
        return chain(request)


def cors_middleware(request: Request, next_fn: Callable) -> Response:
    response = next_fn(request)
    response.headers["Access-Control-Allow-Origin"] = "*"
    return response


def timing_middleware(request: Request, next_fn: Callable) -> Response:
    import time
    start = time.time()
    response = next_fn(request)
    response.headers["X-Response-Time"] = f"{(time.time() - start) * 1000:.2f}ms"
    return response
''',
    "router.py": '''\
from dataclasses import dataclass
from typing import Callable, Dict, List, Optional, Tuple
import re


@dataclass
class Route:
    method: str
    pattern: str
    handler: Callable
    regex: re.Pattern = None

    def __post_init__(self):
        param_pattern = re.sub(r":(\w+)", r"(?P<\\1>[^/]+)", self.pattern)
        self.regex = re.compile(f"^{param_pattern}$")


class Router:
    """URL router with parameter extraction."""

    def __init__(self, prefix: str = ""):
        self.prefix = prefix
        self._routes: List[Route] = []

    def add_route(self, method: str, path: str, handler: Callable) -> None:
        full_path = f"{self.prefix}{path}"
        self._routes.append(Route(method=method.upper(), pattern=full_path, handler=handler))

    def get(self, path: str, handler: Callable) -> None:
        self.add_route("GET", path, handler)

    def post(self, path: str, handler: Callable) -> None:
        self.add_route("POST", path, handler)

    def delete(self, path: str, handler: Callable) -> None:
        self.add_route("DELETE", path, handler)

    def match(self, method: str, path: str) -> Optional[Tuple[Callable, Dict[str, str]]]:
        for route in self._routes:
            if route.method == method.upper():
                m = route.regex.match(path)
                if m:
                    return route.handler, m.groupdict()
        return None

    def list_routes(self) -> List[Dict[str, str]]:
        return [{"method": r.method, "pattern": r.pattern} for r in self._routes]
''',
    "config_loader.py": '''\
from dataclasses import dataclass, field
from typing import Any, Dict, Optional
import os
import json


@dataclass
class Config:
    data: Dict[str, Any] = field(default_factory=dict)

    def get(self, key: str, default: Any = None) -> Any:
        parts = key.split(".")
        current = self.data
        for part in parts:
            if isinstance(current, dict) and part in current:
                current = current[part]
            else:
                return default
        return current

    def set(self, key: str, value: Any) -> None:
        parts = key.split(".")
        current = self.data
        for part in parts[:-1]:
            if part not in current:
                current[part] = {}
            current = current[part]
        current[parts[-1]] = value


class ConfigLoader:
    """Loads configuration from files and environment."""

    def __init__(self, config_dir: str = "/etc/app"):
        self.config_dir = config_dir

    def load(self, env: str = "production") -> Config:
        config = Config()
        base = self._load_file("base.json")
        config.data.update(base)
        env_config = self._load_file(f"{env}.json")
        config.data.update(env_config)
        self._apply_env_overrides(config)
        return config

    def _load_file(self, filename: str) -> Dict[str, Any]:
        path = os.path.join(self.config_dir, filename)
        if os.path.exists(path):
            with open(path) as f:
                return json.load(f)
        return {}

    def _apply_env_overrides(self, config: Config) -> None:
        prefix = "APP_"
        for key, value in os.environ.items():
            if key.startswith(prefix):
                config_key = key[len(prefix):].lower().replace("__", ".")
                config.set(config_key, value)
''',
    "db_pool.py": '''\
from dataclasses import dataclass
from typing import Optional
from contextlib import contextmanager


@dataclass
class Connection:
    id: int
    in_use: bool = False

    def execute(self, sql: str, params: tuple = ()) -> "Connection":
        return self

    def fetchone(self):
        return None

    def fetchall(self):
        return []


class ConnectionPool:
    """Database connection pool with configurable size."""

    def __init__(self, dsn: str, min_size: int = 2, max_size: int = 10):
        self.dsn = dsn
        self.min_size = min_size
        self.max_size = max_size
        self._connections = [Connection(id=i) for i in range(min_size)]

    def acquire(self) -> Connection:
        for conn in self._connections:
            if not conn.in_use:
                conn.in_use = True
                return conn
        if len(self._connections) < self.max_size:
            conn = Connection(id=len(self._connections))
            conn.in_use = True
            self._connections.append(conn)
            return conn
        raise RuntimeError("Connection pool exhausted")

    def release(self, conn: Connection) -> None:
        conn.in_use = False

    @contextmanager
    def connection(self):
        conn = self.acquire()
        try:
            yield conn
        finally:
            self.release(conn)

    def stats(self):
        in_use = sum(1 for c in self._connections if c.in_use)
        return {"total": len(self._connections), "in_use": in_use, "available": len(self._connections) - in_use}


_pool: Optional[ConnectionPool] = None


def init_pool(dsn: str = "sqlite:///app.db", **kwargs) -> ConnectionPool:
    global _pool
    _pool = ConnectionPool(dsn, **kwargs)
    return _pool


def get_connection() -> Connection:
    global _pool
    if _pool is None:
        init_pool()
    return _pool.acquire()
''',
    "event_bus.py": '''\
from dataclasses import dataclass, field
from typing import Callable, Dict, List, Any


@dataclass
class Event:
    type: str
    payload: Any
    source: str = ""


EventHandler = Callable[[Event], None]


class EventBus:
    """Publish-subscribe event bus for decoupled communication."""

    def __init__(self):
        self._handlers: Dict[str, List[EventHandler]] = {}
        self._global_handlers: List[EventHandler] = []

    def subscribe(self, event_type: str, handler: EventHandler) -> None:
        if event_type not in self._handlers:
            self._handlers[event_type] = []
        self._handlers[event_type].append(handler)

    def subscribe_all(self, handler: EventHandler) -> None:
        self._global_handlers.append(handler)

    def publish(self, event: Event) -> int:
        handlers_called = 0
        for handler in self._global_handlers:
            handler(event)
            handlers_called += 1
        if event.type in self._handlers:
            for handler in self._handlers[event.type]:
                handler(event)
                handlers_called += 1
        return handlers_called

    def unsubscribe(self, event_type: str, handler: EventHandler) -> bool:
        if event_type in self._handlers and handler in self._handlers[event_type]:
            self._handlers[event_type].remove(handler)
            return True
        return False

    def clear(self) -> None:
        self._handlers.clear()
        self._global_handlers.clear()
''',
    "task_queue.py": '''\
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional
from enum import Enum
import time
from db_pool import get_connection


class TaskStatus(Enum):
    QUEUED = "queued"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"


@dataclass
class Task:
    id: str
    name: str
    payload: Any
    status: TaskStatus = TaskStatus.QUEUED
    created_at: float = field(default_factory=time.time)
    result: Any = None


class TaskQueue:
    """Persistent task queue with status tracking."""

    def __init__(self, queue_name: str = "default"):
        self.queue_name = queue_name

    def enqueue(self, name: str, payload: Any) -> Task:
        import uuid
        task = Task(id=str(uuid.uuid4()), name=name, payload=payload)
        conn = get_connection()
        conn.execute(
            "INSERT INTO tasks (id, name, queue, status) VALUES (?, ?, ?, ?)",
            (task.id, name, self.queue_name, task.status.value),
        )
        return task

    def dequeue(self) -> Optional[Task]:
        conn = get_connection()
        row = conn.execute(
            "SELECT * FROM tasks WHERE queue = ? AND status = ? ORDER BY created_at LIMIT 1",
            (self.queue_name, TaskStatus.QUEUED.value),
        ).fetchone()
        if row:
            conn.execute("UPDATE tasks SET status = ? WHERE id = ?", (TaskStatus.RUNNING.value, row["id"]))
            return Task(id=row["id"], name=row["name"], payload=None, status=TaskStatus.RUNNING)
        return None

    def complete(self, task_id: str, result: Any = None) -> None:
        conn = get_connection()
        conn.execute("UPDATE tasks SET status = ? WHERE id = ?", (TaskStatus.COMPLETED.value, task_id))

    def fail(self, task_id: str) -> None:
        conn = get_connection()
        conn.execute("UPDATE tasks SET status = ? WHERE id = ?", (TaskStatus.FAILED.value, task_id))
''',
    "file_storage.py": '''\
from dataclasses import dataclass
from typing import Optional
import os
import hashlib


@dataclass
class StoredFile:
    id: str
    filename: str
    content_hash: str
    size_bytes: int
    content_type: str


class FileStorage:
    """Local file storage with content-addressable hashing."""

    def __init__(self, base_path: str = "/var/data/files"):
        self.base_path = base_path

    def store(self, filename: str, data: bytes, content_type: str = "application/octet-stream") -> StoredFile:
        import uuid
        content_hash = hashlib.sha256(data).hexdigest()
        file_id = str(uuid.uuid4())
        dest = os.path.join(self.base_path, content_hash[:2], content_hash)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        with open(dest, "wb") as f:
            f.write(data)
        return StoredFile(
            id=file_id, filename=filename, content_hash=content_hash,
            size_bytes=len(data), content_type=content_type,
        )

    def retrieve(self, content_hash: str) -> Optional[bytes]:
        path = os.path.join(self.base_path, content_hash[:2], content_hash)
        if os.path.exists(path):
            with open(path, "rb") as f:
                return f.read()
        return None

    def delete(self, content_hash: str) -> bool:
        path = os.path.join(self.base_path, content_hash[:2], content_hash)
        if os.path.exists(path):
            os.remove(path)
            return True
        return False

    def exists(self, content_hash: str) -> bool:
        path = os.path.join(self.base_path, content_hash[:2], content_hash)
        return os.path.exists(path)
''',
    "audit_log.py": '''\
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional
from datetime import datetime
from db_pool import get_connection


@dataclass
class AuditEntry:
    id: str
    action: str
    actor: str
    resource: str
    timestamp: datetime
    details: Dict[str, Any] = field(default_factory=dict)


class AuditLog:
    """Immutable audit trail for compliance and debugging."""

    def log(self, action: str, actor: str, resource: str, details: Optional[Dict[str, Any]] = None) -> AuditEntry:
        import uuid
        entry = AuditEntry(
            id=str(uuid.uuid4()),
            action=action,
            actor=actor,
            resource=resource,
            timestamp=datetime.utcnow(),
            details=details or {},
        )
        conn = get_connection()
        conn.execute(
            "INSERT INTO audit_log (id, action, actor, resource, timestamp) VALUES (?, ?, ?, ?, ?)",
            (entry.id, action, actor, resource, entry.timestamp.isoformat()),
        )
        return entry

    def query(self, actor: Optional[str] = None, action: Optional[str] = None, limit: int = 100) -> List[AuditEntry]:
        conn = get_connection()
        sql = "SELECT * FROM audit_log WHERE 1=1"
        params: list = []
        if actor:
            sql += " AND actor = ?"
            params.append(actor)
        if action:
            sql += " AND action = ?"
            params.append(action)
        sql += " ORDER BY timestamp DESC LIMIT ?"
        params.append(limit)
        rows = conn.execute(sql, params).fetchall()
        return [
            AuditEntry(
                id=r["id"], action=r["action"], actor=r["actor"],
                resource=r["resource"], timestamp=datetime.fromisoformat(r["timestamp"]),
            )
            for r in rows
        ]

    def count_actions(self, actor: str) -> int:
        conn = get_connection()
        row = conn.execute("SELECT COUNT(*) as cnt FROM audit_log WHERE actor = ?", (actor,)).fetchone()
        return row["cnt"] if row else 0
''',
}


# Files that use get_connection (for verification)
FILES_WITH_GET_CONNECTION = [
    "auth_service.py", "user_service.py", "order_service.py", "payment_service.py",
    "inventory_service.py", "shipping_service.py", "health_check.py",
    "analytics_service.py", "task_queue.py", "audit_log.py", "db_pool.py",
]


def create(workdir: str) -> None:
    services_dir = os.path.join(workdir, "services")
    os.makedirs(services_dir, exist_ok=True)
    for filename, content in FILES.items():
        path = os.path.join(services_dir, filename)
        with open(path, "w") as f:
            f.write(content)
    print(f"Created {len(FILES)} files in {services_dir}")


def verify(workdir: str) -> None:
    services_dir = os.path.join(workdir, "services")
    passed = 0
    failed = 0
    total = 0

    for filename in sorted(FILES.keys()):
        path = os.path.join(services_dir, filename)
        if not os.path.exists(path):
            print(f"MISSING: {filename}")
            failed += 3
            total += 3
            continue

        with open(path) as f:
            content = f.read()
        lines = content.split("\n")

        # Check 1: Copyright header
        total += 1
        if lines[0] == "# Copyright 2026 Acme Corp":
            passed += 1
        else:
            print(f"FAIL copyright: {filename} (line 1: {lines[0]!r})")
            failed += 1

        # Check 2: Logging setup
        total += 1
        has_import = "import logging" in content
        has_logger = 'logger = logging.getLogger(__name__)' in content
        if has_import and has_logger:
            passed += 1
        else:
            print(f"FAIL logging: {filename} (import={has_import}, logger={has_logger})")
            failed += 1

        # Check 3: Rename get_connection -> acquire_connection
        total += 1
        if filename in FILES_WITH_GET_CONNECTION:
            has_old = "get_connection" in content
            has_new = "acquire_connection" in content
            if has_new and not has_old:
                passed += 1
            elif filename == "db_pool.py":
                # db_pool defines the function, so both def and calls should be renamed
                if has_new and not has_old:
                    passed += 1
                else:
                    print(f"FAIL rename: {filename} (old={has_old}, new={has_new})")
                    failed += 1
            else:
                print(f"FAIL rename: {filename} (old={has_old}, new={has_new})")
                failed += 1
        else:
            # File shouldn't have either
            has_old = "get_connection" in content
            if not has_old:
                passed += 1
            else:
                print(f"FAIL rename: {filename} has unexpected get_connection")
                failed += 1

    print(f"\nResults: {passed}/{total} passed, {failed} failed")
    return passed == total


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: benchmark-setup-scale.py <create|verify> <workdir>")
        sys.exit(1)

    command = sys.argv[1]
    workdir = sys.argv[2]

    if command == "create":
        create(workdir)
    elif command == "verify":
        success = verify(workdir)
        sys.exit(0 if success else 1)
    else:
        print(f"Unknown command: {command}")
        sys.exit(1)
