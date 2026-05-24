from __future__ import annotations
from typhon_runtime import Ok, Err, Result
from typing import NewType
import dataclasses
import json

RequestId = NewType("RequestId", int)


@dataclasses.dataclass(slots=True)
class JsonRpcError:
    code: int
    message: str


type Response = Success | Failure


@dataclasses.dataclass(slots=True)
class Success:
    id: RequestId
    result: dict[str, str]


@dataclasses.dataclass(slots=True)
class Failure:
    id: RequestId
    error: JsonRpcError


@dataclasses.dataclass(slots=True)
class Client:
    next_id: int = 1

    def make_request(
        self, method: str, params: dict[str, str]
    ) -> tuple[RequestId, str]:
        req_id: RequestId = RequestId(self.next_id)
        self.next_id = self.next_id + 1
        payload: dict[str, object] = {
            "jsonrpc": "2.0",
            "id": self.next_id - 1,
            "method": method,
            "params": params,
        }
        return (req_id, json.dumps(payload, sort_keys=True))


def parse_response(text: str, expect_id: RequestId) -> Result[Response, JsonRpcError]:
    try:
        raw: dict[str, object] = json.loads(text)
    except json.JSONDecodeError as e:
        return Err(JsonRpcError(code=-32700, message=f"parse error: {e}"))
    if raw.get("jsonrpc") != "2.0":
        return Err(JsonRpcError(code=-32600, message="not a jsonrpc 2.0 response"))
    if "error" in raw:
        err_dict: dict[str, object] = raw["error"]
        err: JsonRpcError = JsonRpcError(
            code=int(err_dict["code"]), message=str(err_dict["message"])
        )
        return Ok(Failure(id=expect_id, error=err))
    if "result" in raw:
        typed_res: dict[str, str] = {}
        if True:
            res_raw = raw["result"]
            for k, v in res_raw.items():
                typed_res[str(k)] = str(v)
        return Ok(Success(id=expect_id, result=typed_res))
    return Err(JsonRpcError(code=-32603, message="missing result and error"))


def main() -> None:
    client: Client = Client()
    (req_id, request) = client.make_request("get_user", {"id": "42"})
    print(f"request {req_id}: {request}")
    server_reply: str = (
        '{"jsonrpc":"2.0","id":1,"result":{"name":"Ada","email":"ada@example.com"}}'
    )
    match parse_response(server_reply, req_id):
        case Ok(Success(id, result)):
            print(f"success id={id}: {result}")
        case Ok(Failure(id, error)):
            print(f"failure id={id}: code={error.code} msg={error.message}")
        case Err(e):
            print(f"transport error: code={e.code} msg={e.message}")


if __name__ == "__main__":
    main()
