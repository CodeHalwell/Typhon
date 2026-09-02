# VM `argparse`: a faithful subset of CPython's argparse (store / store_true /
# store_false / store_const / append / append_const / extend / count / help /
# version actions, positional and optional arguments, nargs, choices, type,
# default, required, subparsers, argument groups, mutually exclusive groups)
# whose help / usage / error text matches CPython's HelpFormatter for the
# common layouts. Validated against the real module (see validate_argparse.py).
import sys as _sys
import os as _os

SUPPRESS = "==SUPPRESS=="
OPTIONAL = "?"
ZERO_OR_MORE = "*"
ONE_OR_MORE = "+"
REMAINDER = "..."
PARSER = "A..."


def _min_values(action):
    """Values `action` must consume for the parse to succeed."""
    if action.action == "parsers":
        return 1 if action.required else 0
    n = action.nargs
    if n is None or n == ONE_OR_MORE:
        return 1
    if isinstance(n, int):
        return n
    return 0


class ArgumentError(Exception):
    def __init__(self, argument, message):
        self.argument_name = argument
        self.message = message
        self.args = (message,)

    def __str__(self):
        if self.argument_name is None:
            return self.message
        return "argument %s: %s" % (self.argument_name, self.message)


class ArgumentTypeError(Exception):
    pass


class Namespace:
    def __init__(self, **kwargs):
        for name in kwargs:
            setattr(self, name, kwargs[name])

    def __repr__(self):
        items = []
        for k in vars(self):
            items.append("%s=%r" % (k, getattr(self, k)))
        return "Namespace(%s)" % ", ".join(items)

    def __eq__(self, other):
        if not isinstance(other, Namespace):
            return False
        return vars(self) == vars(other)

    def __contains__(self, key):
        return key in vars(self)


class _Action:
    def __init__(self, option_strings, dest, nargs=None, const=None, default=None,
                 type=None, choices=None, required=False, help=None, metavar=None,
                 action="store", version=None):
        self.option_strings = option_strings
        self.dest = dest
        self.nargs = nargs
        self.const = const
        self.default = default
        self.type = type
        self.choices = choices
        self.required = required
        self.help = help
        self.metavar = metavar
        self.action = action
        self.version = version
        self.container = None


class _ArgumentGroup:
    def __init__(self, parser, title=None, description=None):
        self.parser = parser
        self.title = title
        self.description = description
        self.actions = []

    def add_argument(self, *args, **kwargs):
        action = self.parser._make_action(args, kwargs)
        self.actions.append(action)
        self.parser._register(action)
        return action

    def add_mutually_exclusive_group(self, required=False):
        return self.parser.add_mutually_exclusive_group(required=required)


class _MutuallyExclusiveGroup:
    def __init__(self, parser, required=False):
        self.parser = parser
        self.required = required
        self.actions = []

    def add_argument(self, *args, **kwargs):
        action = self.parser._make_action(args, kwargs)
        self.actions.append(action)
        self.parser._register(action)
        if action.option_strings:
            self.parser._optionals_group.actions.append(action)
        else:
            self.parser._positionals_group.actions.append(action)
        return action


class _SubParsersAction:
    def __init__(self, parser, dest, required, help, title, description, metavar):
        self.parser = parser
        self.dest = dest
        self.required = required
        self.help = help
        self.title = title
        self.description = description
        self.metavar = metavar
        self.option_strings = []
        self.nargs = PARSER
        self.default = None
        self.choices = {}
        self.choices_actions = []
        self.type = None
        self.const = None
        self.action = "parsers"
        self.container = None

    def add_parser(self, name, **kwargs):
        if "prog" not in kwargs:
            kwargs["prog"] = "%s %s" % (self.parser._usage_prog_prefix(), name)
        aliases = kwargs.pop("aliases", ())
        help_ = kwargs.pop("help", None)
        sub = ArgumentParser(**kwargs)
        self.choices[name] = sub
        for a in aliases:
            self.choices[a] = sub
        if help_ is not None:
            self.choices_actions.append((name, aliases, help_))
        return sub

    def _metavar_text(self):
        if self.metavar is not None:
            return self.metavar
        return "{%s}" % ",".join(self.choices.keys())


class HelpFormatter:
    def __init__(self, prog, indent_increment=2, max_help_position=24, width=None):
        if width is None:
            width = _terminal_width() - 2
        self._prog = prog
        self._indent_increment = indent_increment
        self._max_help_position = min(max_help_position, max(width - 20, indent_increment * 2))
        self._width = width


class RawDescriptionHelpFormatter(HelpFormatter):
    pass


class RawTextHelpFormatter(RawDescriptionHelpFormatter):
    pass


class ArgumentDefaultsHelpFormatter(HelpFormatter):
    pass


def _terminal_width():
    try:
        cols = _os.environ.get("COLUMNS")
        if cols:
            return int(cols)
    except Exception:
        pass
    return 80


def _wrap(text, width):
    words = text.split()
    lines = []
    cur = ""
    for w in words:
        if not cur:
            cur = w
        elif len(cur) + 1 + len(w) <= width:
            cur = cur + " " + w
        else:
            lines.append(cur)
            cur = w
    if cur:
        lines.append(cur)
    return lines


def _fill_text(text, width, indent):
    text = " ".join(text.strip().split())
    return "\n".join(indent + line for line in _wrap(text, width - len(indent)))


class ArgumentParser:
    def __init__(self, prog=None, usage=None, description=None, epilog=None, parents=(),
                 formatter_class=HelpFormatter, prefix_chars="-", fromfile_prefix_chars=None,
                 argument_default=None, conflict_handler="error", add_help=True,
                 allow_abbrev=True, exit_on_error=True):
        if prog is None:
            prog = _os.path.basename(_sys.argv[0]) if _sys.argv and _sys.argv[0] else ""
            if prog == "":
                prog = "-"
        self.prog = prog
        self.usage = usage
        self.description = description
        self.epilog = epilog
        self.prefix_chars = prefix_chars
        self.argument_default = argument_default
        self.add_help = add_help
        self.allow_abbrev = allow_abbrev
        self.exit_on_error = exit_on_error
        self._actions = []
        self._option_map = {}
        self._defaults = {}
        self._subparsers_action = None
        self._positionals_group = _ArgumentGroup(self, "positional arguments")
        self._optionals_group = _ArgumentGroup(self, "options")
        self._action_groups = [self._positionals_group, self._optionals_group]
        self._mutex_groups = []
        self._pending_sub_tail = []
        if add_help:
            self.add_argument("-h", "--help", action="help", default=SUPPRESS,
                              help="show this help message and exit")
        for parent in parents:
            for a in parent._actions:
                if a.action == "help":
                    continue
                self._actions.append(a)
                for o in a.option_strings:
                    self._option_map[o] = a
                if a.option_strings:
                    self._optionals_group.actions.append(a)
                else:
                    self._positionals_group.actions.append(a)

    # ── declaration ───────────────────────────────────────────────────────
    def _usage_prog_prefix(self):
        parts = [self.prog]
        for a in self._actions:
            if not a.option_strings and a.action != "parsers":
                parts.append(self._format_args(a, a.dest))
        return " ".join(parts)

    def _make_action(self, args, kwargs):
        kwargs = dict(kwargs)
        action = kwargs.pop("action", "store")
        if not args:
            raise TypeError("add_argument() missing argument name")
        if len(args) == 1 and (not isinstance(args[0], str) or args[0][:1] not in self.prefix_chars):
            dest = args[0]
            option_strings = []
            if "dest" in kwargs:
                raise ValueError("dest supplied twice for positional argument")
        else:
            option_strings = list(args)
            for o in option_strings:
                if not isinstance(o, str) or o[:1] not in self.prefix_chars:
                    raise ValueError("invalid option string %r: must start with a character %r" % (o, self.prefix_chars))
            dest = kwargs.pop("dest", None)
            if dest is None:
                longs = [o for o in option_strings if len(o) > 1 and o[1:2] in self.prefix_chars]
                src = longs[0] if longs else option_strings[0]
                dest = src.lstrip(self.prefix_chars).replace("-", "_")
        nargs = kwargs.pop("nargs", None)
        const = kwargs.pop("const", None)
        default = kwargs.pop("default", self._defaults.get(dest, self.argument_default))
        type_ = kwargs.pop("type", None)
        choices = kwargs.pop("choices", None)
        required = kwargs.pop("required", False)
        help_ = kwargs.pop("help", None)
        metavar = kwargs.pop("metavar", None)
        version = kwargs.pop("version", None)
        if kwargs:
            raise TypeError("add_argument() got an unexpected keyword argument %r" % list(kwargs)[0])
        if action in ("store_true", "store_false"):
            if nargs is not None:
                raise ValueError("nargs for store actions must be != 0")
            nargs = 0
            if default is None or default is self.argument_default:
                default = action == "store_false"
            const = action == "store_true"
        elif action == "store_const" or action == "append_const" or action == "count" or action == "help" or action == "version":
            nargs = 0
        elif action == "extend":
            if nargs is None:
                nargs = "+"
        if action == "store" and nargs == 0:
            raise ValueError("nargs for store actions must be != 0; if you have nothing to store, actions such as store true or store const may be more appropriate")
        if not option_strings:
            required = nargs not in (OPTIONAL, ZERO_OR_MORE, REMAINDER)
        a = _Action(option_strings, dest, nargs=nargs, const=const, default=default, type=type_,
                    choices=choices, required=required, help=help_, metavar=metavar, action=action,
                    version=version)
        a.container = self
        return a

    def _register(self, action):
        for o in action.option_strings:
            if o in self._option_map:
                raise ArgumentError(action.option_strings[0], "conflicting option string: %s" % o)
            self._option_map[o] = action
        self._actions.append(action)

    def add_argument(self, *args, **kwargs):
        action = self._make_action(args, kwargs)
        self._register(action)
        if action.option_strings:
            self._optionals_group.actions.append(action)
        else:
            self._positionals_group.actions.append(action)
        return action

    def add_argument_group(self, title=None, description=None):
        g = _ArgumentGroup(self, title, description)
        self._action_groups.append(g)
        return g

    def add_mutually_exclusive_group(self, required=False):
        g = _MutuallyExclusiveGroup(self, required)
        self._mutex_groups.append(g)
        return g

    def add_subparsers(self, **kwargs):
        if self._subparsers_action is not None:
            self.error("cannot have multiple subparser arguments")
        dest = kwargs.pop("dest", SUPPRESS)
        required = kwargs.pop("required", False)
        help_ = kwargs.pop("help", None)
        title = kwargs.pop("title", None)
        description = kwargs.pop("description", None)
        metavar = kwargs.pop("metavar", None)
        kwargs.pop("parser_class", None)
        kwargs.pop("prog", None)
        action = _SubParsersAction(self, dest, required, help_, title, description, metavar)
        action.container = self
        self._subparsers_action = action
        self._actions.append(action)
        if title is not None or description is not None:
            g = self.add_argument_group(title if title is not None else "subcommands", description)
            g.actions.append(action)
        else:
            self._positionals_group.actions.append(action)
        return action

    def set_defaults(self, **kwargs):
        for k in kwargs:
            self._defaults[k] = kwargs[k]
            for a in self._actions:
                if a.dest == k:
                    a.default = kwargs[k]

    def get_default(self, dest):
        for a in self._actions:
            if a.dest == dest and a.default is not SUPPRESS:
                return a.default
        return self._defaults.get(dest, None)

    # ── formatting ────────────────────────────────────────────────────────
    def _metavar(self, action, default):
        if action.metavar is not None:
            return action.metavar
        if action.action == "parsers":
            return action._metavar_text()
        if action.choices is not None:
            return "{%s}" % ",".join(str(c) for c in action.choices)
        return default

    def _format_args(self, action, default_metavar):
        mv = self._metavar(action, default_metavar)
        if action.action == "parsers":
            return "%s ..." % mv
        n = action.nargs
        if n is None:
            return mv
        if n == OPTIONAL:
            return "[%s]" % mv
        if n == ZERO_OR_MORE:
            return "[%s ...]" % mv
        if n == ONE_OR_MORE:
            return "%s [%s ...]" % (mv, mv)
        if n == REMAINDER:
            return "..."
        if n == PARSER:
            return "%s ..." % mv
        if n == SUPPRESS:
            return ""
        return " ".join([mv] * n)

    def format_usage(self):
        if self.usage is not None:
            return "usage: %s\n" % (self.usage % {"prog": self.prog} if "%(prog)" in self.usage else self.usage)
        prog = self.prog
        opt_parts = []
        pos_parts = []
        for a in self._actions:
            if getattr(a, "help", None) is SUPPRESS and a.action != "help":
                continue
            if a.option_strings:
                opt = a.option_strings[0]
                if a.nargs == 0:
                    part = opt
                else:
                    part = "%s %s" % (opt, self._format_args(a, a.dest.upper()))
                if not a.required:
                    part = "[%s]" % part
                opt_parts.append(part)
            else:
                pos_parts.append(self._format_args(a, a.dest))
        prefix = "usage: "
        text_width = _terminal_width() - 2
        usage = " ".join([prog] + opt_parts + pos_parts)
        if len(prefix) + len(usage) > text_width:
            def get_lines(parts, indent, pfx=None):
                lines = []
                line = []
                indent_length = len(indent)
                if pfx is not None:
                    line_len = len(pfx) - 1
                else:
                    line_len = indent_length - 1
                for part in parts:
                    if line_len + 1 + len(part) > text_width and line:
                        lines.append(indent + " ".join(line))
                        line = []
                        line_len = indent_length - 1
                    line.append(part)
                    line_len += len(part) + 1
                if line:
                    lines.append(indent + " ".join(line))
                if pfx is not None:
                    lines[0] = lines[0][indent_length:]
                return lines
            if len(prefix) + len(prog) <= 0.75 * text_width:
                indent = " " * (len(prefix) + len(prog) + 1)
                if opt_parts:
                    lines = get_lines([prog] + opt_parts, indent, prefix)
                    lines.extend(get_lines(pos_parts, indent))
                elif pos_parts:
                    lines = get_lines([prog] + pos_parts, indent, prefix)
                else:
                    lines = [prog]
            else:
                indent = " " * len(prefix)
                parts = opt_parts + pos_parts
                lines = get_lines(parts, indent)
                if len(lines) > 1:
                    lines = []
                    lines.extend(get_lines(opt_parts, indent))
                    lines.extend(get_lines(pos_parts, indent))
                lines = [prog] + lines
            usage = "\n".join(lines)
        return prefix + usage + "\n"

    def _invocation(self, action):
        if not action.option_strings:
            return self._metavar(action, action.dest)
        if action.nargs == 0:
            return ", ".join(action.option_strings)
        # CPython 3.13 shows the metavar once, after the last option string.
        args = self._format_args(action, action.dest.upper())
        return "%s %s" % (", ".join(action.option_strings), args)

    def _expand_help(self, action):
        text = action.help
        if text is None:
            return None
        params = {"prog": self.prog, "default": action.default, "dest": action.dest}
        if action.choices is not None:
            params["choices"] = ", ".join(str(c) for c in action.choices)
        if "%(" in text:
            try:
                text = text % params
            except Exception:
                pass
        return text

    def format_help(self):
        width = _terminal_width() - 2
        max_help_position = min(24, max(width - 20, 4))
        rows_by_group = []
        max_len = 0
        for g in self._action_groups:
            rows = []
            for a in g.actions:
                if getattr(a, "help", None) is SUPPRESS:
                    continue
                inv = self._invocation(a)
                rows.append((2, inv, self._expand_help(a) if a.action != "parsers" else a.help))
                max_len = max(max_len, len(inv) + 2)
                if a.action == "parsers":
                    for name, aliases, help_ in a.choices_actions:
                        sub_inv = name if not aliases else "%s (%s)" % (name, ", ".join(aliases))
                        rows.append((4, sub_inv, help_))
                        max_len = max(max_len, len(sub_inv) + 4)
            rows_by_group.append((g, rows))
        help_position = min(max_len + 2, max_help_position)
        help_width = max(width - help_position, 11)
        out = [self.format_usage()]
        if self.description:
            out.append("\n" + _fill_text(self.description, width, "") + "\n")
        for g, rows in rows_by_group:
            if not rows:
                continue
            out.append("\n%s:\n" % g.title)
            if g.description:
                out.append(_fill_text(g.description, width, "  ") + "\n")
            for indent, inv, help_ in rows:
                action_width = help_position - indent - 2
                if help_ is None or help_ == "":
                    out.append("%s%s\n" % (" " * indent, inv))
                    continue
                lines = _wrap(help_, help_width)
                if len(inv) <= action_width:
                    first = "%s%s  %s\n" % (" " * indent, inv.ljust(action_width), lines[0])
                    rest = lines[1:]
                else:
                    first = "%s%s\n" % (" " * indent, inv)
                    rest = lines
                out.append(first)
                for line in rest:
                    out.append("%s%s\n" % (" " * help_position, line))
        if self.epilog:
            out.append("\n" + _fill_text(self.epilog, width, "") + "\n")
        return "".join(out)

    def print_usage(self, file=None):
        if file is None:
            file = _sys.stdout
        file.write(self.format_usage())

    def print_help(self, file=None):
        if file is None:
            file = _sys.stdout
        file.write(self.format_help())

    def exit(self, status=0, message=None):
        if message:
            _sys.stderr.write(message)
        _sys.exit(status)

    def error(self, message):
        self.print_usage(_sys.stderr)
        self.exit(2, "%s: error: %s\n" % (self.prog, message))

    # ── parsing ───────────────────────────────────────────────────────────
    def _get_value(self, action, arg_string):
        type_func = action.type
        if type_func is None:
            return arg_string
        try:
            result = type_func(arg_string)
        except ArgumentTypeError as e:
            raise ArgumentError(self._action_name(action), str(e))
        except (TypeError, ValueError):
            name = getattr(type_func, "__name__", repr(type_func))
            raise ArgumentError(self._action_name(action), "invalid %s value: %r" % (name, arg_string))
        return result

    def _check_value(self, action, value):
        if action.choices is not None and value not in action.choices:
            choices = ", ".join(map(str, action.choices))
            raise ArgumentError(self._action_name(action), "invalid choice: %r (choose from %s)" % (value, choices))

    def _action_name(self, action):
        if action.option_strings:
            return "/".join(action.option_strings)
        if action.metavar not in (None, SUPPRESS):
            return action.metavar
        if action.dest not in (None, SUPPRESS):
            return action.dest
        if action.choices:
            return "{" + ",".join(action.choices) + "}"
        return None

    def _match_option(self, arg):
        if arg in self._option_map:
            return self._option_map[arg], None
        if "=" in arg:
            name, explicit = arg.split("=", 1)
            if name in self._option_map:
                return self._option_map[name], explicit
        if self.allow_abbrev and len(arg) > 2 and arg[1:2] in self.prefix_chars:
            name = arg.split("=", 1)[0]
            matches = [o for o in self._option_map if o.startswith(name) and len(o) > 2]
            if len(matches) == 1:
                explicit = arg.split("=", 1)[1] if "=" in arg else None
                return self._option_map[matches[0]], explicit
            if len(matches) > 1:
                self.error("ambiguous option: %s could match %s" % (name, ", ".join(matches)))
        if len(arg) > 2 and arg[0] in self.prefix_chars and arg[1] not in self.prefix_chars:
            short = arg[:2]
            if short in self._option_map:
                a = self._option_map[short]
                if a.nargs == 0:
                    return a, ("-" + arg[2:], "cluster")
                return a, arg[2:]
        return None, None

    def _is_option_like(self, arg):
        if not arg or arg[0] not in self.prefix_chars:
            return False
        if arg in self._option_map:
            return True
        if len(arg) == 1:
            return False
        if "=" in arg and arg.split("=", 1)[0] in self._option_map:
            return True
        if self._looks_negative_number(arg):
            return False
        if " " in arg:
            return False
        return True

    def _take(self, action, args, pos, explicit):
        n = action.nargs
        if n == 0:
            if explicit is not None and not isinstance(explicit, tuple):
                raise ArgumentError(self._action_name(action), "ignored explicit argument %r" % explicit)
            return [], pos
        if explicit is not None:
            vals = [explicit]
            if n in (None, OPTIONAL) or n == 1:
                return vals, pos
            raise ArgumentError(self._action_name(action), "ignored explicit argument %r" % explicit)
        avail = []
        p = pos
        while p < len(args) and not self._is_option_like(args[p]) and args[p] != "--":
            avail.append(args[p])
            p += 1
        if n is None:
            if not avail:
                raise ArgumentError(self._action_name(action), "expected one argument")
            return avail[:1], pos + 1
        if n == OPTIONAL:
            if not avail:
                return None, pos
            return avail[:1], pos + 1
        if n == ZERO_OR_MORE:
            return avail, p
        if n == ONE_OR_MORE:
            if not avail:
                raise ArgumentError(self._action_name(action), "expected at least one argument")
            return avail, p
        if n == REMAINDER:
            return args[pos:], len(args)
        if isinstance(n, int):
            if len(avail) < n:
                raise ArgumentError(self._action_name(action), "expected %d argument%s" % (n, "" if n == 1 else "s"))
            return avail[:n], pos + n
        return avail, p

    def _store(self, ns, action, values):
        a = action.action
        if a == "store":
            if action.nargs in (None, OPTIONAL) and values is not None:
                setattr(ns, action.dest, values[0] if values else None)
            elif values is None:
                setattr(ns, action.dest, action.const if action.const is not None else action.default)
            else:
                setattr(ns, action.dest, values)
        elif a == "store_true" or a == "store_false" or a == "store_const":
            setattr(ns, action.dest, action.const)
        elif a == "append":
            cur = getattr(ns, action.dest, None)
            cur = list(cur) if cur is not None else []
            if action.nargs in (None, OPTIONAL):
                cur.append(values[0] if values else action.const)
            else:
                cur.append(values)
            setattr(ns, action.dest, cur)
        elif a == "append_const":
            cur = getattr(ns, action.dest, None)
            cur = list(cur) if cur is not None else []
            cur.append(action.const)
            setattr(ns, action.dest, cur)
        elif a == "extend":
            cur = getattr(ns, action.dest, None)
            cur = list(cur) if cur is not None else []
            cur.extend(values)
            setattr(ns, action.dest, cur)
        elif a == "count":
            cur = getattr(ns, action.dest, None)
            setattr(ns, action.dest, (cur or 0) + 1)
        elif a == "help":
            self.print_help()
            self.exit()
        elif a == "version":
            _sys.stdout.write((action.version or "") + "\n")
            self.exit()

    def _convert(self, action, values):
        if values is None:
            return None
        out = []
        for v in values:
            cv = self._get_value(action, v)
            self._check_value(action, cv)
            out.append(cv)
        return out

    def parse_known_args(self, args=None, namespace=None):
        if args is None:
            args = list(_sys.argv[1:])
        else:
            args = list(args)
        if namespace is None:
            namespace = Namespace()
        for a in self._actions:
            if a.action == "parsers":
                if a.dest is not SUPPRESS and not hasattr(namespace, a.dest):
                    setattr(namespace, a.dest, None)
                continue
            if a.dest is not SUPPRESS and a.default is not SUPPRESS and not hasattr(namespace, a.dest):
                setattr(namespace, a.dest, a.default)
        for k in self._defaults:
            if not hasattr(namespace, k):
                setattr(namespace, k, self._defaults[k])
        try:
            extras = self._parse(args, namespace)
        except ArgumentError as e:
            if self.exit_on_error:
                self.error(str(e))
            raise
        for a in self._actions:
            if a.action != "parsers" and a.dest is not SUPPRESS and isinstance(a.default, str) and getattr(namespace, a.dest, None) is a.default and a.type is not None:
                setattr(namespace, a.dest, self._get_value(a, a.default))
        return namespace, extras

    def _positionals_before_subparsers(self):
        n = 0
        for a in self._actions:
            if a.action == "parsers":
                return n
            if not a.option_strings:
                if a.nargs is None or a.nargs == OPTIONAL:
                    n += 1
                elif isinstance(a.nargs, int):
                    n += a.nargs
                else:
                    n += 1
        return n

    def _parse(self, args, ns):
        positionals = [a for a in self._actions if not a.option_strings]
        seen = set()
        extras = []
        # Positional values are gathered into one flat list. CPython instead
        # matches each *contiguous run* of them, so positionals interleaved
        # with options (`a --flag b c`) are grouped differently there.
        pos_values = []
        i = 0
        stop_options = False
        while i < len(args):
            arg = args[i]
            if not stop_options and arg == "--":
                stop_options = True
                i += 1
                continue
            action = None
            explicit = None
            if not stop_options and arg and arg[0] in self.prefix_chars and len(arg) > 1 and not self._looks_negative_number(arg):
                action, explicit = self._match_option(arg)
                if action is None:
                    extras.append(arg)
                    i += 1
                    continue
            if action is not None:
                i += 1
                if isinstance(explicit, tuple):
                    args.insert(i, explicit[0])
                    explicit = None
                values, i = self._take(action, args, i, explicit)
                seen.add(id(action))
                self._store(ns, action, self._convert(action, values))
                for g in self._mutex_groups:
                    if action in g.actions:
                        for other in g.actions:
                            if other is not action and id(other) in seen:
                                raise ArgumentError(self._action_name(action), "not allowed with argument %s" % self._action_name(other))
                continue
            if self._subparsers_action is not None and len(pos_values) >= self._positionals_before_subparsers():
                # This token names the sub-command: the rest of argv is its.
                pos_values.append(arg)
                self._pending_sub_tail = args[i + 1:]
                break
            pos_values.append(arg)
            i += 1
        consumed = 0
        for index, a in enumerate(positionals):
            remaining = pos_values[consumed:]
            # CPython matches every positional's `nargs` against argv in one
            # regular expression, so a greedy `?`/`*`/`+`/`...` gives back
            # whatever the positionals after it still need. Hold that many
            # values in reserve rather than swallowing them here.
            reserve = 0
            for later in positionals[index + 1:]:
                reserve += _min_values(later)
            available = len(remaining) - reserve
            if available < 0:
                available = 0
            if a.action == "parsers":
                if not remaining:
                    if a.required:
                        raise ArgumentError(None, "the following arguments are required: %s" % (a.dest if a.dest is not SUPPRESS else a._metavar_text()))
                    break
                name = remaining[0]
                if name not in a.choices:
                    raise ArgumentError(self._action_name(a) if a.dest is not SUPPRESS else None, "invalid choice: %r (choose from %s)" % (name, ", ".join(a.choices.keys())))
                if a.dest is not SUPPRESS:
                    setattr(ns, a.dest, name)
                sub = a.choices[name]
                sub_argv = self._pending_sub_tail
                self._pending_sub_tail = []
                sub_ns, sub_extras = sub.parse_known_args(sub_argv, ns)
                extras.extend(sub_extras)
                consumed = len(pos_values)
                seen.add(id(a))
                break
            n = a.nargs
            if n is None:
                if not remaining:
                    continue
                vals = remaining[:1]
                consumed += 1
            elif n == OPTIONAL:
                if not available:
                    self._store(ns, a, None)
                    seen.add(id(a))
                    continue
                vals = remaining[:1]
                consumed += 1
            elif n == ZERO_OR_MORE:
                vals = remaining[:available]
                consumed += len(vals)
                if not vals and a.default is not None:
                    seen.add(id(a))
                    continue
            elif n == ONE_OR_MORE:
                if not remaining:
                    continue
                # `+` still takes one value even when a later positional then
                # goes unfilled: CPython reports only the later one missing.
                vals = remaining[:available] if available else remaining[:1]
                consumed += len(vals)
            elif n == REMAINDER:
                vals = remaining[:available]
                consumed += len(vals)
            elif isinstance(n, int):
                if len(remaining) < n:
                    if remaining:
                        raise ArgumentError(self._action_name(a), "expected %d argument%s" % (n, "" if n == 1 else "s"))
                    continue
                vals = remaining[:n]
                consumed += n
            else:
                vals = remaining[:available]
                consumed += len(vals)
            seen.add(id(a))
            self._store(ns, a, self._convert(a, vals))
        extras.extend(pos_values[consumed:])
        missing = []
        for a in self._actions:
            if id(a) in seen:
                continue
            if a.action == "parsers":
                if a.required:
                    missing.append(a.dest if a.dest is not SUPPRESS else a._metavar_text())
                continue
            if a.required:
                missing.append(self._action_name(a))
        for g in self._mutex_groups:
            if g.required and not any(id(x) in seen for x in g.actions):
                names = " ".join(self._action_name(x) for x in g.actions)
                raise ArgumentError(None, "one of the arguments %s is required" % names)
        if missing:
            raise ArgumentError(None, "the following arguments are required: %s" % ", ".join(missing))
        return extras

    def _looks_negative_number(self, arg):
        if len(arg) < 2 or arg[0] != "-":
            return False
        try:
            float(arg)
        except ValueError:
            return False
        for o in self._option_map:
            try:
                float(o)
                return False
            except ValueError:
                pass
        return True

    def parse_args(self, args=None, namespace=None):
        ns, extras = self.parse_known_args(args, namespace)
        if extras:
            self.error("unrecognized arguments: %s" % " ".join(extras))
        return ns

    def parse_intermixed_args(self, args=None, namespace=None):
        return self.parse_args(args, namespace)
