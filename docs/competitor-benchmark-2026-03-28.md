# RustQueue Competitor Benchmark (2026-03-28)

## Method

Sequential local benchmark on one machine.
Each metric runs **5000 operations** per system.
Runs per system/metric: **3** (reported value = median).
RustQueue redb durability mode: **immediate**.
RustQueue remove_on_complete: **False**.
RustQueue TCP batch size: **50**.
RustQueue write coalescing: **False**.
RustQueue hybrid storage: **True**.

Metrics:
- `produce_ops_s`: enqueue throughput
- `consume_ops_s`: dequeue(+ack where relevant) throughput from a prefilled queue
- `end_to_end_ops_s`: enqueue + dequeue(+ack where relevant) in the same loop

Systems:
- `rustqueue_http`: RustQueue REST API
- `rustqueue_tcp`: RustQueue TCP protocol
- `redis_list`: Redis LPUSH/RPOP
- `rabbitmq`: RabbitMQ basic_publish/basic_get/basic_ack
- `bullmq`: BullMQ on Redis
- `celery`: Celery on Redis

## Results

| System | Produce ops/s | Consume ops/s | End-to-end ops/s |
|--------|---------------:|--------------:|-----------------:|
| rustqueue_tcp | 47,129 | 38,048 | 22,685 |
| redis_list | 9,337 | 9,511 | 4,951 |
| rabbitmq | 47,588 | 5,367 | 4,686 |
| bullmq | 7,559 | 6,690 | 1,978 |
| celery | 3,168 | 1,589 | 893 |
| rustqueue_http | 2,415 | 1,236 | 819 |

## Winners

- Produce winner: `rabbitmq`
- Consume winner: `rustqueue_tcp`
- End-to-end winner: `rustqueue_tcp`

## Notes

- This compares local defaults and protocol-level behavior, not managed cloud offerings.
- RustQueue storage is `redb` with durability mode `immediate`; competitor durability semantics vary.
- RustQueue jobs are enqueued with `remove_on_complete=false` in this run.
- RustQueue TCP benchmark used `batch_size=50` (`push_batch`/`ack_batch` when >1).
- RustQueue write coalescing was **disabled** (buffers single push/ack into batched flushes when enabled).
- RustQueue hybrid storage was **enabled** (in-memory DashMap + periodic redb snapshots when enabled).
- Celery and BullMQ include worker/runtime overhead in consume and end-to-end measurements.
- For stricter apples-to-apples durability, run an additional profile with explicit fsync/persistence settings on each system.
