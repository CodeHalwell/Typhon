# `property(fget, fset, fdel, doc)` called as a function — the decorator
# form `@property` is handled natively by the class builder. A data
# descriptor: the VM's attribute protocol calls `__get__` on read through an
# instance (and with `obj=None` through the class), and `__set__` /
# `__delete__` on assignment / deletion through an instance.


class property:
    def __init__(self, fget=None, fset=None, fdel=None, doc=None):
        self.fget = fget
        self.fset = fset
        self.fdel = fdel
        if doc is None and fget is not None:
            doc = getattr(fget, "__doc__", None)
        self.__doc__ = doc
        self.__name__ = getattr(fget, "__name__", None) if fget is not None else None

    def __set_name__(self, owner, name):
        self.__name__ = name

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        if self.fget is None:
            raise AttributeError(
                f"property {self.__name__!r} of {type(obj).__name__!r} object has no getter"
            )
        return self.fget(obj)

    def __set__(self, obj, value):
        if self.fset is None:
            raise AttributeError(
                f"property {self.__name__!r} of {type(obj).__name__!r} object has no setter"
            )
        self.fset(obj, value)

    def __delete__(self, obj):
        if self.fdel is None:
            raise AttributeError(
                f"property {self.__name__!r} of {type(obj).__name__!r} object has no deleter"
            )
        self.fdel(obj)

    def getter(self, fget):
        return type(self)(fget, self.fset, self.fdel, self.__doc__)

    def setter(self, fset):
        return type(self)(self.fget, fset, self.fdel, self.__doc__)

    def deleter(self, fdel):
        return type(self)(self.fget, self.fset, fdel, self.__doc__)
