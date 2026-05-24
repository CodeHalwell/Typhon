from __future__ import annotations
import dataclasses
from collections import deque


@dataclasses.dataclass(slots=True)
class Graph:
    adj: dict[str, list[str]] = dataclasses.field(default_factory=dict)

    def add_edge(self, u: str, v: str) -> None:
        if u not in self.adj:
            self.adj[u] = []
        if v not in self.adj:
            self.adj[v] = []
        self.adj[u].append(v)
        self.adj[v].append(u)

    def neighbours(self, node: str) -> list[str]:
        if node in self.adj:
            return self.adj[node]
        return []


def bfs(g: Graph, start: str) -> list[str]:
    order: list[str] = []
    seen: set[str] = {start}
    q: deque[str] = deque([start])
    while len(q) > 0:
        node: str = q.popleft()
        order.append(node)
        for nb in g.neighbours(node):
            if nb not in seen:
                seen.add(nb)
                q.append(nb)
    return order


def dfs(g: Graph, start: str) -> list[str]:
    order: list[str] = []
    seen: set[str] = set()
    stack: list[str] = [start]
    while len(stack) > 0:
        node: str = stack.pop()
        if node in seen:
            continue
        seen.add(node)
        order.append(node)
        for nb in g.neighbours(node):
            if nb not in seen:
                stack.append(nb)
    return order


def main() -> None:
    g: Graph = Graph()
    edges: list[tuple[str, str]] = [
        ("A", "B"),
        ("A", "C"),
        ("B", "D"),
        ("C", "D"),
        ("C", "E"),
        ("D", "F"),
        ("E", "F"),
    ]
    for u, v in edges:
        g.add_edge(u, v)
    print("BFS from A:", bfs(g, "A"))
    print("DFS from A:", dfs(g, "A"))


if __name__ == "__main__":
    main()
