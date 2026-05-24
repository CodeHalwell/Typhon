from __future__ import annotations
import dataclasses
from datetime import datetime, timezone
from sqlalchemy import (
    String,
    Integer,
    Float,
    ForeignKey,
    DateTime,
    create_engine,
    select,
    func,
)
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship, Session


@dataclasses.dataclass(slots=True)
class Base(DeclarativeBase):
    pass


@dataclasses.dataclass(slots=True)
class Customer(Base):
    __tablename__ = "customers"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    name: Mapped[str] = mapped_column(String(120), nullable=False)
    email: Mapped[str] = mapped_column(String(200), unique=True, nullable=False)
    orders: Mapped[list["Order"]] = relationship(
        back_populates="customer", cascade="all, delete-orphan"
    )


@dataclasses.dataclass(slots=True)
class Order(Base):
    __tablename__ = "orders"
    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    customer_id: Mapped[int] = mapped_column(ForeignKey("customers.id"), nullable=False)
    sku: Mapped[str] = mapped_column(String(40), nullable=False)
    amount: Mapped[float] = mapped_column(Float, nullable=False)
    placed_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    customer: Mapped["Customer"] = relationship(back_populates="orders")


def seed(session: Session) -> None:
    ada = Customer(name="Ada Lovelace", email="ada@example.com")
    grace = Customer(name="Grace Hopper", email="grace@example.com")
    now = datetime.now(timezone.utc)
    ada.orders = [
        Order(sku="widget", amount=29.99, placed_at=now),
        Order(sku="gadget", amount=149.5, placed_at=now),
    ]
    grace.orders = [Order(sku="widget", amount=29.99, placed_at=now)]
    session.add_all([ada, grace])
    session.commit()


def top_customers(session: Session, limit: int = 5) -> list[tuple[str, float]]:
    stmt = (
        select(Customer.name, func.sum(Order.amount).label("total"))
        .join(Order, Order.customer_id == Customer.id)
        .group_by(Customer.id)
        .order_by(func.sum(Order.amount).desc())
        .limit(limit)
    )
    return [(row.name, float(row.total)) for row in session.execute(stmt).all()]


def find_customer(session: Session, email: str) -> Customer | None:
    stmt = select(Customer).where(Customer.email == email)
    return session.scalars(stmt).first()


def main() -> None:
    engine = create_engine("sqlite:///:memory:", echo=False, future=True)
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        seed(session)
        for name, total in top_customers(session):
            print(f"  {name:20s} ${total:.2f}")
        ada: Customer | None = find_customer(session, "ada@example.com")
        if ada is not None:
            print(f"\n{ada.name} has {len(ada.orders)} orders")


if __name__ == "__main__":
    main()
