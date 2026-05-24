from __future__ import annotations
from pydantic import ConfigDict
import dataclasses
from fastapi import FastAPI, HTTPException, Depends, Query
from pydantic import BaseModel, Field
import uvicorn


class NewTask(BaseModel):
    model_config = ConfigDict(extra="forbid")
    title: str
    priority: int = Field(default=1, ge=1, le=5)


class Task(BaseModel):
    model_config = ConfigDict(extra="forbid")
    id: int
    title: str
    priority: int
    done: bool


@dataclasses.dataclass(slots=True)
class TaskStore:
    items: dict[int, Task]
    next_id: int

    def add(self, new: NewTask) -> Task:
        task: Task = Task(
            id=self.next_id, title=new.title, priority=new.priority, done=False
        )
        self.items[task.id] = task
        self.next_id = self.next_id + 1
        return task

    def get(self, id: int) -> Task | None:
        return self.items.get(id)

    def list(self, min_priority: int) -> list[Task]:
        return sorted(
            (t for t in self.items.values() if t.priority >= min_priority),
            key=lambda t: (-t.priority, t.id),
        )

    def mark_done(self, id: int) -> Task | None:
        task: Task | None = self.items.get(id)
        if task is None:
            return None
        updated: Task = Task(
            id=task.id, title=task.title, priority=task.priority, done=True
        )
        self.items[id] = updated
        return updated


store: TaskStore = TaskStore(items={}, next_id=1)
app: FastAPI = FastAPI(title="Typhon Tasks")


def get_store() -> TaskStore:
    return store


@app.post("/tasks", response_model=Task, status_code=201)
def create_task(payload: NewTask, s: TaskStore = Depends(get_store)) -> Task:
    return s.add(payload)


@app.get("/tasks", response_model=list[Task])
def list_tasks(
    min_priority: int = Query(default=1, ge=1, le=5), s: TaskStore = Depends(get_store)
) -> list[Task]:
    return s.list(min_priority)


@app.get("/tasks/{task_id}", response_model=Task)
def get_task(task_id: int, s: TaskStore = Depends(get_store)) -> Task:
    found: Task | None = s.get(task_id)
    if found is None:
        raise HTTPException(status_code=404, detail=f"no such task: {task_id}")
    return found


@app.post("/tasks/{task_id}/done", response_model=Task)
def complete_task(task_id: int, s: TaskStore = Depends(get_store)) -> Task:
    updated: Task | None = s.mark_done(task_id)
    if updated is None:
        raise HTTPException(status_code=404, detail=f"no such task: {task_id}")
    return updated


def main() -> None:
    uvicorn.run(app, host="127.0.0.1", port=8000)


if __name__ == "__main__":
    main()
