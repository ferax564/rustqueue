# RustQueue Competitor Benchmark (2026-02-06)

## Method

Sequential local benchmark on one machine.
Each metric runs **500 operations** per system.
Runs per system/metric: **2** (reported value = median).
RustQueue redb durability mode: **immediate**.
RustQueue remove_on_complete: **False**.
RustQueue TCP batch size: **50**.

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
| rustqueue_tcp | 10,929 | 5,970 | 3,692 |
| redis_list | 3,005 | 3,448 | 1,704 |
| rabbitmq | 27,637 | 1,922 | 1,519 |
| bullmq | 1,728 | 1,804 | 545 |
| celery | 983 | 513 | 342 |
| rustqueue_http | 245 | 105 | 73 |

## Winners

- Produce winner: `rabbitmq`
- Consume winner: `rustqueue_tcp`
- End-to-end winner: `rustqueue_tcp`

## Notes

- This compares local defaults and protocol-level behavior, not managed cloud offerings.
- RustQueue storage is `redb` with durability mode `immediate`; competitor durability semantics vary.
- RustQueue jobs are enqueued with `remove_on_complete=false` in this run.
- RustQueue TCP benchmark used `batch_size=50` (`push_batch`/`ack_batch` when >1).
- Celery and BullMQ include worker/runtime overhead in consume and end-to-end measurements.
- For stricter apples-to-apples durability, run an additional profile with explicit fsync/persistence settings on each system.
