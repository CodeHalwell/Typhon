from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import ast
import json
import os
from typing import Callable
from anthropic import Anthropic


def _eval_arith(node: object) -> float:
    if True:
        if isinstance(node, ast.Constant):
            v = node.value
            if isinstance(v, bool):
                raise ValueError("booleans are not numbers")
            if isinstance(v, int) or isinstance(v, float):
                return float(v)
            raise ValueError(f"non-numeric constant: {v!r}")
        if isinstance(node, ast.BinOp):
            left: float = _eval_arith(node.left)
            right: float = _eval_arith(node.right)
            if isinstance(node.op, ast.Add):
                return left + right
            if isinstance(node.op, ast.Sub):
                return left - right
            if isinstance(node.op, ast.Mult):
                return left * right
            if isinstance(node.op, ast.Div):
                return left / right
            raise ValueError(f"forbidden operator: {type(node.op).__name__}")
        if isinstance(node, ast.UnaryOp):
            if isinstance(node.op, ast.USub):
                return -_eval_arith(node.operand)
            if isinstance(node.op, ast.UAdd):
                return _eval_arith(node.operand)
            raise ValueError(f"forbidden unary op")
        raise ValueError(f"forbidden node: {type(node).__name__}")
    raise RuntimeError("unreachable")


@dataclasses.dataclass(slots=True)
class Tool:
    name: str
    description: str
    input_schema: dict[str, object]
    run: Callable[[dict[str, object]], str]


@dataclasses.dataclass(slots=True)
class AgentError:
    stage: str
    message: str


@dataclasses.dataclass(slots=True)
class Agent:
    client: Anthropic
    model: str
    tools: dict[str, Tool]
    system: str
    history: list[dict[str, object]]

    def register(self, tool: Tool) -> None:
        self.tools[tool.name] = tool

    def tool_schemas(self) -> list[dict[str, object]]:
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            }
            for t in self.tools.values()
        ]

    def step(self, max_turns: int = 6) -> Result[str, AgentError]:
        turn: int = 0
        while turn < max_turns:
            try:
                resp = self.client.messages.create(
                    model=self.model,
                    max_tokens=1024,
                    system=self.system,
                    tools=self.tool_schemas(),
                    messages=self.history,
                )
            except Exception as e:
                return Err(AgentError(stage="api", message=str(e)))
            self.history.append({"role": "assistant", "content": resp.content})
            if resp.stop_reason != "tool_use":
                text_parts: list[str] = []
                for text_block in resp.content:
                    if text_block.type == "text":
                        text_parts.append(text_block.text)
                return Ok("".join(text_parts))
            tool_results: list[dict[str, object]] = []
            for tool_block in resp.content:
                if tool_block.type == "tool_use":
                    tool: Tool | None = self.tools.get(tool_block.name)
                    out: str = (
                        tool.run(dict(tool_block.input))
                        if tool is not None
                        else f'{{"error": "unknown tool {tool_block.name}"}}'
                    )
                    tool_results.append(
                        {
                            "type": "tool_result",
                            "tool_use_id": tool_block.id,
                            "content": out,
                        }
                    )
            self.history.append({"role": "user", "content": tool_results})
            turn = turn + 1
        return Err(AgentError(stage="loop", message=f"max_turns={max_turns} exhausted"))

    def ask(self, question: str) -> Result[str, AgentError]:
        self.history.append({"role": "user", "content": question})
        return self.step()


def search_tool() -> Tool:
    docs: dict[str, str] = {
        "typhon": "Statically-typed superset of Python that compiles to .py.",
        "tyc": "The single-binary Typhon compiler. Subcommands: build, check, fmt, lsp.",
        "result": "Result[T, E] models recoverable failures; ? short-circuits.",
    }

    def run(args: dict[str, object]) -> str:
        query: str = str(args["query"]).lower()
        hits: list[dict[str, str]] = []
        for k, v in docs.items():
            if query in k or query in v.lower():
                hits.append({"key": k, "text": v})
        return json.dumps({"hits": hits})

    return Tool(
        name="search_docs",
        description="Search internal Typhon docs by keyword.",
        input_schema={
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        },
        run=run,
    )


def calc_tool() -> Tool:

    def run(args: dict[str, object]) -> str:
        expr: str = str(args["expression"])
        try:
            tree = ast.parse(expr, mode="eval")
            return json.dumps({"value": _eval_arith(tree.body)})
        except (SyntaxError, ValueError, ZeroDivisionError) as e:
            return json.dumps({"error": str(e)})

    return Tool(
        name="calculate",
        description="Evaluate a numeric expression.",
        input_schema={
            "type": "object",
            "properties": {"expression": {"type": "string"}},
            "required": ["expression"],
        },
        run=run,
    )


def make_agent(client: Anthropic) -> Agent:
    agent: Agent = Agent(
        client=client,
        model="claude-opus-4-7",
        tools={},
        system="You are a helpful research agent. Use tools when they would beat guessing.",
        history=[],
    )
    agent.register(search_tool())
    agent.register(calc_tool())
    return agent


def main() -> None:
    key: str | None = os.environ.get("ANTHROPIC_API_KEY")
    if key is None:
        print("ANTHROPIC_API_KEY not set — skipping")
        return
    agent: Agent = make_agent(Anthropic(api_key=key))
    for q in [
        "What does the Typhon compiler do? Quote the docs.",
        "Compute 17 * (3 + 4) and explain.",
    ]:
        print(f"\n> {q}")
        match agent.ask(q):
            case Ok(reply):
                print(reply)
            case Err(e):
                print(f"agent error: {e.stage}/{e.message}")


if __name__ == "__main__":
    main()
