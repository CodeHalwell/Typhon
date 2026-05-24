from __future__ import annotations
import dataclasses

APP_NAME: str = "research-assistant"
PORT: int = 8080
LOG_LEVEL: str = "info"
IS_PROD: bool = False
SUPPORTED_LANGS: list[str] = ["en", "fr", "de", "es", "ja"]
SHIPS_AUTH: bool = False
SHIPS_BILLING: bool = False


@dataclasses.dataclass(slots=True)
class Config:
    app_name: str
    port: int
    log_level: str
    is_prod: bool
    auth_enabled: bool
    billing_enabled: bool


def build_config() -> Config:
    return Config(
        app_name=APP_NAME,
        port=PORT,
        log_level=LOG_LEVEL,
        is_prod=IS_PROD,
        auth_enabled=SHIPS_AUTH,
        billing_enabled=SHIPS_BILLING,
    )


def main() -> None:
    cfg: Config = build_config()
    print(f"{cfg.app_name} on port {cfg.port}")
    print(f"  log level:      {cfg.log_level}")
    print(f"  production:     {cfg.is_prod}")
    print(f"  auth feature:   {cfg.auth_enabled}")
    print(f"  billing feature:{cfg.billing_enabled}")
    print(f"  langs:          {SUPPORTED_LANGS}")


if __name__ == "__main__":
    main()
