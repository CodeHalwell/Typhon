from __future__ import annotations
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import
from io import StringIO

pd = __typhon_lazy_import("pandas")
RAW_CSV: str = """date,user,product,units,price
2026-05-01,ada,widget,3,9.99
2026-05-01,grace,gadget,,49.99
2026-05-02,ada,WIDGET ,2,9.99
2026-05-03,linus,thingy,5,14.50
2026-05-03,ada,gadget,1,49.99
2026-05-04,Grace,widget,4,9.99
"""


def load_dataframe(text: str) -> pd.DataFrame:
    return pd.read_csv(StringIO(text), parse_dates=["date"])


def clean(df: pd.DataFrame) -> pd.DataFrame:
    out = df.copy()
    out["product"] = out["product"].str.strip().str.lower()
    out["user"] = out["user"].str.strip().str.lower()
    out["units"] = out["units"].fillna(1).astype(int)
    out["revenue"] = out["units"] * out["price"]
    return out


def daily_revenue(df: pd.DataFrame) -> pd.DataFrame:
    return df.groupby("date", as_index=False)["revenue"].sum()


def top_users(df: pd.DataFrame, n: int = 3) -> pd.DataFrame:
    return (
        df.groupby("user", as_index=False)["revenue"]
        .sum()
        .sort_values("revenue", ascending=False)
        .head(n)
    )


def pivot_units(df: pd.DataFrame) -> pd.DataFrame:
    return df.pivot_table(
        index="user", columns="product", values="units", aggfunc="sum", fill_value=0
    )


def main() -> None:
    raw: pd.DataFrame = load_dataframe(RAW_CSV)
    print("raw:")
    print(raw)
    clean_df: pd.DataFrame = clean(raw)
    print("""
cleaned:""")
    print(clean_df)
    print("""
daily revenue:""")
    print(daily_revenue(clean_df))
    print("""
top users:""")
    print(top_users(clean_df))
    print("""
pivot (units per user x product):""")
    print(pivot_units(clean_df))


if __name__ == "__main__":
    main()
