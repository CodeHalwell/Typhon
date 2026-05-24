from __future__ import annotations


def quicksort(xs: list[int]) -> list[int]:
    if len(xs) < 2:
        return xs
    pivot: int = xs[len(xs) // 2]
    lo: list[int] = [x for x in xs if x < pivot]
    mid: list[int] = [x for x in xs if x == pivot]
    hi: list[int] = [x for x in xs if x > pivot]
    return quicksort(lo) + mid + quicksort(hi)


def merge(a: list[int], b: list[int]) -> list[int]:
    out: list[int] = []
    i: int = 0
    j: int = 0
    while i < len(a) and j < len(b):
        if a[i] <= b[j]:
            out.append(a[i])
            i = i + 1
        else:
            out.append(b[j])
            j = j + 1
    out.extend(a[i:])
    out.extend(b[j:])
    return out


def mergesort(xs: list[int]) -> list[int]:
    if len(xs) < 2:
        return xs
    mid: int = len(xs) // 2
    return merge(mergesort(xs[:mid]), mergesort(xs[mid:]))


def insertion_sort(xs: list[int]) -> list[int]:
    out: list[int] = list(xs)
    i: int = 1
    while i < len(out):
        key: int = out[i]
        j: int = i - 1
        while j >= 0 and out[j] > key:
            out[j + 1] = out[j]
            j = j - 1
        out[j + 1] = key
        i = i + 1
    return out


def main() -> None:
    data: list[int] = [9, 3, 7, 1, 8, 2, 6, 5, 4, 0]
    print("input:        ", data)
    print("quicksort:    ", quicksort(data))
    print("mergesort:    ", mergesort(data))
    print("insertion:    ", insertion_sort(data))


if __name__ == "__main__":
    main()
