from __future__ import annotations
import dataclasses
import json
import logging
import sys
from datetime import datetime, timezone


@dataclasses.dataclass(slots=True)
class JsonFormatter(logging.Formatter):
    pass

    def format(self, record: logging.LogRecord) -> str:
        payload: dict[str, object] = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "level": record.levelname,
            "logger": record.name,
            "msg": record.getMessage(),
        }
        if record.exc_info is not None:
            payload["exc"] = self.formatException(record.exc_info)
        return json.dumps(payload)


def configure_logging(level: str = "INFO", as_json: bool = False) -> None:
    root: logging.Logger = logging.getLogger()
    root.setLevel(level)
    handler = logging.StreamHandler(sys.stdout)
    if as_json:
        handler.setFormatter(JsonFormatter())
    else:
        handler.setFormatter(
            logging.Formatter("%(asctime)s [%(levelname)-5s] %(name)s: %(message)s")
        )
    root.handlers = [handler]


def process_batch(log: logging.Logger, items: list[int]) -> int:
    log.info("processing batch", extra={"size": len(items)})
    total: int = 0
    for i, item in enumerate(items):
        try:
            total = total + 100 // item
        except ZeroDivisionError:
            log.warning(f"skipping zero at index {i}")
        except Exception as e:
            log.exception(f"unexpected error at index {i}")
    log.info(f"batch done: total={total}")
    return total


def main() -> None:
    configure_logging(level="DEBUG", as_json=False)
    log: logging.Logger = logging.getLogger("examples.processor")
    log.debug("startup")
    log.info("running batch")
    process_batch(log, [10, 5, 0, 4, 2])
    configure_logging(level="INFO", as_json=True)
    json_log: logging.Logger = logging.getLogger("examples.json")
    json_log.info("now logging structured")
    json_log.warning("watch out", extra={"code": 42})


if __name__ == "__main__":
    main()
