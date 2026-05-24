from __future__ import annotations
import dataclasses

type Cell = Empty | Played
type Mark = X | O


@dataclasses.dataclass(slots=True)
class Empty:
    pass


@dataclasses.dataclass(slots=True)
class Played:
    mark: Mark


@dataclasses.dataclass(slots=True)
class X:
    pass


@dataclasses.dataclass(slots=True)
class O:
    pass


@dataclasses.dataclass(slots=True)
class Board:
    cells: list[list[Cell]] = dataclasses.field(default_factory=list)

    def render(self) -> str:
        rows: list[str] = []
        for row in self.cells:
            chars: list[str] = []
            for cell in row:
                match cell:
                    case Empty():
                        chars.append(".")
                    case Played(mark):
                        match mark:
                            case X():
                                chars.append("X")
                            case O():
                                chars.append("O")
            rows.append(" ".join(chars))
        return """
""".join(rows)

    def place(self, row: int, col: int, mark: Mark) -> bool:
        cell: Cell = self.cells[row][col]
        match cell:
            case Empty():
                self.cells[row][col] = Played(mark=mark)
                return True
            case Played(_):
                return False
        return False


def new_board() -> Board:
    return Board(cells=[[Empty(), Empty(), Empty()] for _ in range(3)])


def same_mark(a: Cell, b: Cell, c: Cell) -> Mark | None:
    match a:
        case Empty():
            return None
        case Played(ma):
            match b:
                case Empty():
                    return None
                case Played(mb):
                    match c:
                        case Empty():
                            return None
                        case Played(mc):
                            if type(ma) is type(mb) and type(mb) is type(mc):
                                return ma
                            return None


def winner(b: Board) -> Mark | None:
    for i in range(3):
        row_win: Mark | None = same_mark(b.cells[i][0], b.cells[i][1], b.cells[i][2])
        if row_win is not None:
            return row_win
        col_win: Mark | None = same_mark(b.cells[0][i], b.cells[1][i], b.cells[2][i])
        if col_win is not None:
            return col_win
    diag1: Mark | None = same_mark(b.cells[0][0], b.cells[1][1], b.cells[2][2])
    if diag1 is not None:
        return diag1
    return same_mark(b.cells[0][2], b.cells[1][1], b.cells[2][0])


def main() -> None:
    b: Board = new_board()
    moves: list[tuple[int, int, Mark]] = [
        (0, 0, X()),
        (1, 1, O()),
        (0, 1, X()),
        (2, 0, O()),
        (0, 2, X()),
    ]
    for r, c, m in moves:
        b.place(r, c, m)
    print(b.render())
    match winner(b):
        case None:
            print("no winner")
        case w:
            print(f"winner: {type(w).__name__}")


if __name__ == "__main__":
    main()
