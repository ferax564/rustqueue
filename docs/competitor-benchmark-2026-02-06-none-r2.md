# RustQueue Competitor Benchmark (2026-02-06)

## Method

Sequential local benchmark on one machine.
Each metric runs **500 operations** per system.
Runs per system/metric: **2** (reported value = median).
RustQueue redb durability mode: **none**.
RustQueue remove_on_complete: **False**.

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
| rabbitmq | 25,386 | 2,120 | 2,403 |
| redis_list | 3,316 | 5,292 | 2,248 |
| bullmq | 2,737 | 2,874 | 836 |
| celery | 1,737 | 649 | 381 |
| rustqueue_http | 229 | 87 | 60 |
| rustqueue_tcp | 343 | 72 | 57 |

## Winners

- Produce winner: `rabbitmq`
- Consume winner: `redis_list`
- End-to-end winner: `rabbitmq`

## Notes

- This compares local defaults and protocol-level behavior, not managed cloud offerings.
- RustQueue storage is `redb` with durability mode `none`; competitor durability semantics vary.
- RustQueue jobs are enqueued with `remove_on_complete=false` in this run.
- Celery and BullMQ include worker/runtime overhead in consume and end-to-end measurements.
- For stricter apples-to-apples durability, run an additional profile with explicit fsync/persistence settings on each system.
