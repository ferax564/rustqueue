# RustQueue Competitor Benchmark (2026-02-06)

## Method

Sequential local benchmark on one machine.
Each metric runs **500 operations** per system.
RustQueue redb durability mode: **eventual**.

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
| rabbitmq | 44,751 | 4,609 | 4,170 |
| redis_list | 6,084 | 7,536 | 3,916 |
| bullmq | 5,115 | 4,853 | 1,706 |
| celery | 2,704 | 1,356 | 884 |
| rustqueue_http | 282 | 110 | 83 |
| rustqueue_tcp | 367 | 116 | 83 |

## Winners

- Produce winner: `rabbitmq`
- Consume winner: `redis_list`
- End-to-end winner: `rabbitmq`

## Notes

- This compares local defaults and protocol-level behavior, not managed cloud offerings.
- RustQueue storage is `redb` with durability mode `eventual`; competitor durability semantics vary.
- Celery and BullMQ include worker/runtime overhead in consume and end-to-end measurements.
- For stricter apples-to-apples durability, run an additional profile with explicit fsync/persistence settings on each system.
