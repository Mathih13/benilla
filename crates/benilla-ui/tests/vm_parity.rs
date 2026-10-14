//! The script host on either VM: every case runs through `UiScript`, and its answer must equal
//! the one the C Lua build recorded in `vm_parity.golden`. `BENILLA_BLESS=1` rewrites the file;
//! only the C Lua build may bless it.

use benilla_ui::script::UiScript;

/// Serializers both VMs run: `S(...)` writes values, `E(f, ...)` writes a protected call.
const PRELUDE: &str = r#"
local function S1(v)
  local t = type(v)
  if t == "string" then return string.format("%q", v)
  elseif t == "number" or t == "boolean" or t == "nil" then return tostring(v)
  else return t end
end
local function S(...)
  local o = {}
  for i = 1, arg.n do o[i] = S1(arg[i]) end
  return table.concat(o, ",", 1, arg.n)
end
local function pack(...) return arg end
local function E(f, ...)
  local r = pack(pcall(f, unpack(arg)))
  if r[1] then
    local o = {}
    for i = 2, r.n do o[i - 1] = S1(r[i]) end
    return "ok " .. table.concat(o, ",", 1, r.n - 1)
  end
  local t = type(r[2])
  -- A table or function error value prints its type: its address differs every run.
  return "err " .. ((t == "table" or t == "function") and t or tostring(r[2]))
end
"#;

const CASES: &[(&str, &str)] = &[
    ("arith", "return S(1+2, 10/4, 2^10, -3, 7-2*3, 3-(-2), 2^-1, -2^2)"),
    ("numfmt", "return S(1e15, 1e100, 0.1, 1/3, 123456789012345, -0.5, 100, 2^53)"),
    ("coerce", r#"return S("10"+5, "3"*"4", 10 .. 20, 1.5 .. "", " 7 " + 0, -"2")"#),
    ("arith_sides", r#"local mt = {}
        for _, e in {"__add", "__sub", "__mul", "__div"} do
          -- A fresh local per pass: 5.0 shares the loop variable itself.
          local name = e
          mt[e] = function(a, b) return name .. ":" .. type(a) .. "," .. type(b) end
        end
        local t = setmetatable({}, mt)
        return S(1 + t, t + 1, 2 - t, t - 2, 3 * t, t * 3, 4 / t, t / 4, t + t)"#),
    ("arith_const_errors", r#"return S(E(function() local s = "x" return 2 * s end),
                 E(function() local x return 1 + x end),
                 E(function() local s = "x" return s - 1 end), E(function() return 1 / {} end),
                 E(function() local t = {} return 2 - t.y end))"#),
    ("arith_regs", r#"local a, b = 7, "3" return S(a + b, a - b, a * b, a / b, b - a)"#),
    ("numeric_for", r#"local o = {}
        for i = 1, 3 do table.insert(o, i) end
        for i = 3, 1, -1 do table.insert(o, i) end
        for i = 1, 2, 0.5 do table.insert(o, i) end
        for i = 1, "2" do table.insert(o, i) end
        local st = 2 for i = 1, 5, st do table.insert(o, i) end
        for i = 5, 1 do table.insert(o, "never") end
        return table.concat(o, " ")"#),
    ("numeric_for_limit_error", r#"return E(function() for i = 1, "x" do end end)"#),
    ("numeric_for_start_error", r#"return S(E(function() for i = "a", 2 do end end), E(function() for i = 1, 2, {} do end end))"#),
    ("coerce_hex", r#"return E(function() return "0x10" + 0 end)"#),
    ("tonumber", r#"return S(tonumber("12"), tonumber("1e2"), tonumber("x"), tonumber("ff", 16), tonumber("777", 8), tonumber("  5  "), tonumber(nil))"#),
    ("tonumber_hex", r#"return S(tonumber("0x1F"))"#),
    ("div0", "return S(1/0 > 1e308, -1/0 < 0, 0/0 ~= 0/0)"),
    ("mod", "return S(math.mod(7, 3), math.mod(-7, 3), mod(7.5, 2), math.floor(-3.5), math.ceil(-3.5))"),
    ("cmp", r#"return S(1 < 2, "a" < "b", "abc" < "abd", "Z" < "a", "" < "a", 2 <= 2, 3 >= 4, "10" < "9")"#),
    ("cmp_err", r#"return E(function() return 1 < "2" end)"#),
    ("cmp_err2", r#"return E(function() return {} < {} end)"#),
    ("eq", r#"return S(1 == 1.0, "1" == 1, nil == false, {} == {}, "a" == "a")"#),
    ("concat", r#"return S("a" .. "b" .. 1 .. 2.5, "x" .. "")"#),
    ("concat_err", r#"return E(function() local t = {} return "a" .. t end)"#),
    ("sub", r#"local s = "hello world" return S(string.sub(s, 1, 5), strsub(s, -5), strsub(s, 3), strsub(s, 0), strsub(s, 5, 2), strsub(s, -100, 2), strsub(s, 2, 100))"#),
    ("byte_char", r#"return S(string.byte("A"), strbyte("abc", 2), strbyte("abc", -1), strbyte("abc", 10), string.char(72, 105))"#),
    ("case", r#"return S(string.upper("abcXYZ!"), strlower("HeLLo"), string.len("\0ab"), string.rep("ab", 3), string.rep("x", 0))"#),
    ("find", r#"return S(string.find("hello world", "wor"), strfind("hello", "l"), strfind("hello", "xyz"), strfind("a.b", ".", 1, true), strfind("hello", "l", 4), strfind("hello", "", 10))"#),
    ("find_pat", r#"return S(strfind("key = value", "(%w+)%s*=%s*(%w+)"), strfind("  x", "^%s*"), strfind("abc", "b()"), strfind("[test]", "%[(.-)%]"))"#),
    ("find_anchor", r#"return S(strfind("hello", "^h"), strfind("hello", "^e"), strfind("hello", "o$"), strfind("a$b", "$b"))"#),
    ("classes", r#"return S(strfind("abc123", "%d+"), strfind("a b", "%s"), strfind("x_y", "%p"), strfind("AbC", "%u%l%u"), strfind("\t", "%c"), strfind("ff", "%x+"), strfind("a1", "%W"))"#),
    ("sets", r#"return S(strfind("hello", "[aeiou]"), strfind("hello", "[^hel]"), strfind("a-b", "[%-]"), strfind("x]", "[]]"), strfind("5", "[0-9]"))"#),
    ("quantifiers", r#"return S(strfind("aaab", "a-b"), strfind("aaab", "a*"), strfind("b", "a?b"), strfind("<a><b>", "<(.-)>"), strfind("<a><b>", "<(.*)>"))"#),
    ("balance", r#"return S(strfind("f(a(b)c)d", "%b()"), strfind("THE (quick) fox", "%f[%a]%a+", 5))"#),
    ("backref", r#"return S(strfind('say "hi" now', "([\"'])(.-)%1"))"#),
    ("gsub", r#"return S(string.gsub("hello world", "o", "0"), gsub("abc", "%w", "%0%0"), gsub("hello", "l+", function(s) return "[" .. s .. "]" end), gsub("abc", "", "-"), gsub("a b c", " ", "", 1))"#),
    ("gsub_basic", r#"return S(string.gsub("hello world", "o", "0"), gsub("hello", "l+", function(s) return "[" .. s .. "]" end), gsub("abc", "", "-"), gsub("a b c", " ", "", 1), gsub("$1.50", "%$", "USD"))"#),
    ("gsub_caps", r#"return S(gsub("John Smith", "(%w+) (%w+)", "%2, %1"), gsub("x=1, y=2", "(%w+)=(%w+)", "%2=%1"), gsub("abc", "(b)", "%%"))"#),
    ("gsub_nil_fn", r#"return S(gsub("a1b2", "%d", function(d) if d == "1" then return "one" end end))"#),
    ("gsub_anchor", r#"return S(gsub("aaa", "^a", "b"), gsub("hello", "^", ">"))"#),
    ("gfind", r#"local o = {} for w in string.gfind("one two  three", "%a+") do table.insert(o, w) end for k, v in string.gfind("a=1,b=2", "(%w+)=(%w+)") do table.insert(o, k .. v) end return table.concat(o, "|")"#),
    ("gfind_empty", r#"local o = {} for w in string.gfind("abc", "x*") do table.insert(o, "[" .. w .. "]") end return table.concat(o)"#),
    ("pat_errors", r#"return E(function() return strfind("a", "[a") end) .. E(function() return strfind("a", "%") end) .. E(function() return strfind("a", "(a") end) .. E(function() return gsub("a", "a", "%2") end)"#),
    ("format", r#"return S(string.format("%d %5d %-5d| %05d %+d", 42, 42, 42, 42, 42), format("%x %X %o %#x", 255, 255, 8, 255), format("%s %10s %-4s| %.2s", "hi", "right", "l", "abc"))"#),
    ("format_float", r#"return S(format("%f %.2f %10.3f %e %g %g %g", 3.14159, 2.5, 1/3, 12345.678, 0.0001, 1e20, 123456789))"#),
    ("format_q", r#"return format("%q", "a\"b\\c\nd\0e")"#),
    ("format_c", r#"return S(format("%c%c%c", 76, 117, 97), format("%5.1f%%", 99.5))"#),
    ("format_err", r#"return E(function() return format("%d", "x") end) .. E(function() return format("%y", 1) end)"#),
    ("ctor", "local t = {1, 2, 3, x = 4, [10] = 5; 6} return S(t[1], t[2], t[3], t.x, t[10], t[4], getn(t))"),
    ("ctor_semicolon", "local t = {a = 1;; b = 2} return S(t.a, t.b)"),
    ("getn", "local t = {1, 2, nil, 4} local u = {n = 7} return S(getn(t) >= 2, getn(u), table.getn({}), getn({1, 2, 3}))"),
    ("setn", "local t = {} table.setn(t, 5) local u = {n = 1} table.setn(u, 3) return S(getn(t), u.n, t.n)"),
    ("insert_remove", "local t = {} tinsert(t, 'a') tinsert(t, 'c') tinsert(t, 2, 'b') local r = tremove(t, 1) return S(r, t[1], t[2], t[3], getn(t), tremove({}))"),
    ("concat_tbl", "return S(table.concat({1, 2, 3}, ', '), table.concat({'a', 'b'}), table.concat({1, 2, 3}, '-', 2, 3))"),
    ("concat_err_2", "return E(function() return table.concat({1, {}, 3}) end)"),
    ("sort", "local t = {5, 2, 8, 1, 9, 3} table.sort(t) local u = {'b', 'a', 'c'} sort(u, function(a, b) return a > b end) return table.concat(t, ',') .. '|' .. table.concat(u, ',')"),
    ("sort_many", "local t = {} for i = 1, 100 do t[i] = math.mod(i * 37, 101) end table.sort(t) local ok = true for i = 2, 100 do if t[i-1] > t[i] then ok = false end end return S(ok, t[1], t[100])"),
    ("foreach", "local o = {} foreachi({'a', 'b', 'c'}, function(i, v) table.insert(o, i .. v) end) local r = foreachi({1, 2, 3}, function(i, v) if v == 2 then return 'stop' end end) return table.concat(o, ',') .. S(r)"),
    ("unpack", "return S(unpack({1, 2, 3})) .. '|' .. S(unpack({n = 2, 'a', 'b', 'c'}))"),
    ("next", "local t = {} t.x = 1 local k, v = next(t) return S(k, v, next({}), next(t, 'x'))"),
    ("next_err", "return E(function() return next({}, 'nope') end)"),
    ("ipairs", "local o = {} for i, v in ipairs({'a', 'b', nil, 'd'}) do table.insert(o, i .. v) end return table.concat(o, ',')"),
    ("pairs_array", "local o = {} for k, v in pairs({10, 20, 30}) do table.insert(o, k .. '=' .. v) end return table.concat(o, ',')"),
    ("generic_for_table", "local s = 0 for k, v in {1, 2, 3} do s = s + v end return S(s)"),
    ("nil_key", "return E(function() local t = {} t[nil] = 1 end)"),
    ("rawops", "local t = setmetatable({}, {__index = function() return 'meta' end, __newindex = function() end}) t.x = 1 rawset(t, 'y', 2) return S(t.x, rawget(t, 'x'), t.y, rawget(t, 'y'), rawequal(t, t))"),
    ("index_chain", "local base = {a = 1} local mid = setmetatable({b = 2}, {__index = base}) local t = setmetatable({}, {__index = mid}) return S(t.a, t.b, t.c)"),
    ("newindex", "local log = {} local t = setmetatable({}, {__newindex = function(t, k, v) rawset(t, k, v * 2) end}) t.x = 5 t.x = 7 return S(t.x)"),
    ("arith_mt", "local mt = {__add = function(a, b) return 'add' end, __unm = function() return 'neg' end, __concat = function(a, b) return 'cat' end} local t = setmetatable({}, mt) return S(t + 1, 1 + t, -t, t .. 'x', 'x' .. t)"),
    ("eq_mt", "local mt = {__eq = function() return true end} local a, b = setmetatable({}, mt), setmetatable({}, mt) local c = setmetatable({}, {__eq = function() return true end}) return S(a == b, a == c, a ~= b)"),
    ("lt_mt", "local mt = {__lt = function(a, b) return a.v < b.v end} local a, b = setmetatable({v = 1}, mt), setmetatable({v = 2}, mt) return S(a < b, b < a, a > b, a <= b)"),
    ("call_mt", "local t = setmetatable({}, {__call = function(self, a, b) return a + b end}) return S(t(2, 3))"),
    ("tostring_mt", "local t = setmetatable({}, {__tostring = function() return 'custom' end}) return tostring(t)"),
    ("protected_mt", "local t = setmetatable({}, {__metatable = 'locked'}) return S(getmetatable(t)) .. E(function() setmetatable(t, {}) end)"),
    ("pow_global", "local old = __pow __pow = function(a, b) return 'p' end local r = 2 ^ 3 __pow = old return S(r, 2 ^ 3)"),
    ("varargs", "local function f(...) return arg.n, arg[1], arg[2] end return S(f()) .. '|' .. S(f(1, nil, 3)) .. '|' .. S(f('a'))"),
    ("varargs_named", "local function f(a, ...) return a, arg.n end return S(f(1, 2, 3))"),
    ("multret", "local function f() return 1, 2, 3 end local t = {f(), f()} return S(getn(t), (f()), select and 1 or 0)"),
    ("multret_adjust", "local function f() return 1, 2 end local a, b, c = f() local d, e = (f()) return S(a, b, c, d, e)"),
    ("closures", "local function counter() local n = 0 return function() n = n + 1 return n end end local c1, c2 = counter(), counter() c1() c1() return S(c1(), c2())"),
    ("shared_upvalue", "local a, b do local x = 0 a = function() x = x + 1 return x end b = function() return x end end a() a() return S(b())"),
    ("recursion", "local function fib(n) if n < 2 then return n end return fib(n-1) + fib(n-2) end return S(fib(20))"),
    ("tailcall", "local function loop(n) if n == 0 then return 'done' end return loop(n - 1) end return S(loop(10000))"),
    ("method", "local obj = {v = 5} function obj:get(x) return self.v + x end return S(obj:get(1), obj.get(obj, 2))"),
    ("nested_fn_names", "local t = {a = {}} function t.a.f() return 1 end function t.a:g() return self == t.a end return S(t.a.f(), t.a:g())"),
    ("numfor", "local o = {} for i = 1, 3 do table.insert(o, i) end for i = 3, 1, -1 do table.insert(o, i) end for i = 1, 0 do table.insert(o, 'never') end for i = 1, 2, 0.5 do table.insert(o, i) end return table.concat(o, ',')"),
    ("numfor_closures", "local fs = {} for i = 1, 3 do fs[i] = function() return i end end return S(fs[1](), fs[2](), fs[3]())"),
    ("genfor_closures", "local fs = {} for k, v in ipairs({'a', 'b'}) do fs[k] = function() return v end end return S(fs[1](), fs[2]())"),
    ("body_local_closures", "local fs = {} for i = 1, 3 do local j = i fs[i] = function() return j end end return S(fs[1](), fs[2](), fs[3]())"),
    ("numfor_assign", "local o = {} for i = 1, 5 do table.insert(o, i) i = i + 1 end return table.concat(o, ',')"),
    ("genfor_control", "local o = {} for k, v in pairs({a = 1}) do table.insert(o, k) end return table.concat(o)"),
    ("for_errors", "return E(function() for i = 'x', 2 do end end) .. E(function() for i = 1, 'y' do end end)"),
    ("while_repeat", "local i, o = 0, {} while i < 3 do i = i + 1 table.insert(o, i) end repeat i = i - 1 table.insert(o, i) until i == 0 return table.concat(o, ',')"),
    ("repeat_scope", "local x = 'outer' local n = 0 repeat local x = 'inner' n = n + 1 until x == 'outer' or n > 2 return S(n)"),
    ("break", "local o = {} for i = 1, 10 do if i > 3 then break end table.insert(o, i) end while true do break end return table.concat(o, ',')"),
    ("break_closure", "local fs = {} local i = 0 while true do i = i + 1 local j = i fs[i] = function() return j end if i == 2 then break end end return S(fs[1](), fs[2]())"),
    ("err_index_global", "return E(function() return undefined_thing.x end)"),
    ("err_index_local", "return E(function() local t return t.x end)"),
    ("err_index_field", "return E(function() local t = {} return t.a.b end)"),
    ("err_index_upvalue", "local up return E(function() return up.x end)"),
    ("err_call", "return E(function() undefined_fn() end) .. E(function() local t = {} t.m() end) .. E(function() local t = {} t:m() end)"),
    ("err_arith", "return E(function() local x return x + 1 end) .. E(function() return {} * 2 end)"),
    ("error_fn", "return E(function() error('boom') end) .. E(function() error('noloc', 0) end) .. string.sub(E(function() error({}) end), 1, 11) .. E(function() error(42) end)"),
    ("error_level2", "local function f() error('deep', 2) end return E(function() f() end)"),
    ("assert", "return E(function() assert(false) end) .. E(function() assert(nil, 'msg') end) .. S(assert(1, 2, 3))"),
    ("bad_arg", "return E(function() string.sub() end) .. E(function() strfind(nil, 'x') end) .. E(function() setmetatable(1, {}) end) .. E(function() math.floor('x') end)"),
    ("bad_self", "local t = {f = string.len} return E(function() return t:f() end)"),
    ("xpcall", "local ok, msg = xpcall(function() error('x') end, function(e) return 'handled: ' .. e end) return S(ok, msg) .. '|' .. S(xpcall(function() return 1, 2 end, print))"),
    ("pcall_args", "return S(pcall(function(a, b) return a + b end, 1, 2))"),
    ("nested_pcall", "return E(function() local ok, e = pcall(error, 'inner') error('outer:' .. e, 0) end)"),
    ("fenv", "local env = {x = 'env'} local function f() return x end setfenv(f, env) return S(f(), getfenv(f) == env, getfenv(0) == _G, getfenv() == _G)"),
    ("fenv_level", "x = 'global' local function f() setfenv(1, {x = 'set'}) return x end return S(f())"),
    ("loadstring", "local f = loadstring('return 1 + 1') local g, e = loadstring('return +') return S(f(), g, e)"),
    ("loadstring_name", "local f = loadstring('error(\"x\")', '=named') return E(f)"),
    ("globals", "return S(type(_G), type(print), type(select), type(coroutine), type(string.gmatch), type(math.fmod), type(gcinfo), type(bit), type(getn), type(strfind), type(sin))"),
    ("type", "return S(type(nil), type(1), type('s'), type({}), type(print), type(type), type(true))"),
    ("tostring", "return S(tostring(nil), tostring(true), tostring(12), tostring('x'), string.sub(tostring({}), 1, 7), string.sub(tostring(print), 1, 10))"),
    ("bit", "return S(bit.band(12, 10), bit.bor(12, 10), bit.bxor(12, 10), bit.bnot(0), bit.lshift(1, 4), bit.rshift(256, 4), bit.arshift(-16, 2), bit.lshift(1, 32), bit.mod(7, 3))"),
    ("math", "return S(math.abs(-3), math.max(1, 5, 3), math.min(4, 2), math.sqrt(16), math.floor(2.7), math.pi > 3.14, math.frexp(8), math.ldexp(0.5, 4), math.deg(math.pi))"),
    ("random", "math.randomseed(42) local a = math.random(100) math.randomseed(42) return S(a == math.random(100), math.random() < 1, math.random(5, 5))"),
    // ── What the current VM's 5.1 accepts, which 5.0 does not ──
    ("hex_literals", "return S(0x10, 0xff + 1, 0XA)"),
    ("long_brackets", "return S([=[a]]b]=], [==[x]=]y]==], [[plain]])"),
    ("string_dump", "return S(type(string.dump), E(function() return type(string.dump(function() end)) end))"),
    ("numeric_for_fresh_var", "local fs = {} for i = 1, 3 do fs[i] = function() return i end end return S(fs[1](), fs[2](), fs[3]())"),
    ("generic_for_fresh_var", "local fs = {} for i, v in ipairs({10, 20}) do fs[i] = function() return v end end return S(fs[1](), fs[2]())"),
    ("for_var_assigned", "local s = 0 for i = 1, 5 do s = s + i i = i + 10 end return S(s)"),
    ("until_sees_body_locals", "local n = 0 repeat local stop = n >= 2 n = n + 1 until stop or n > 5 return S(n)"),
    ("pow_metamethod", "local t = setmetatable({}, {__pow = function(a, b) return 'mm' end}) return S(E(function() return t ^ 2 end), E(function() return 2 ^ t end))"),
    ("upvalue_error_name", "local u return S(E(function() return u + 1 end), E(function() return u.x end), E(function() u() end))"),
    ("vararg_expr", "return S(E(loadstring, 'return function(...) return ... end'))"),
    // ── Numbers as the C library reads and writes them ──
    ("nonfinite_print", "return S(1/0, -1/0, string.format('%f %g %e %5.1f', 1/0, -1/0, 1/0, -1/0), tostring(1/0) .. '', 1e308 * 10)"),
    ("nan_print", "local n = 0/0 return S(tostring(n) == tostring(n), string.format('%d', 3.7), string.format('%x', 255), string.format('%5.2s|', 'abc'))"),
    ("tonumber_edges", r#"return S(tonumber("inf"), tonumber("-inf"), tonumber("nan") ~= nil, tonumber("1e"), tonumber("0x"), tonumber(" 0x1A "), tonumber("0x1p4"), tonumber("1e+"), tonumber(".5"), tonumber("5."), tonumber("- 1"), tonumber("1 2"), tonumber(""), tonumber("  "), tonumber("0x10", 10), tonumber("10", 36), tonumber("z", 36))"#),
    ("tonumber_base_edges", r#"return S(tonumber("-1", 16), tonumber("0x10", 16), tonumber(" ff ", 16), tonumber("ffffffffffffffffffff", 16), tonumber("1z", 36), tonumber("8", 8), tonumber("", 16), tonumber("-ff", 16), tonumber("+7", 8))"#),
    ("numeral_lexing", "return S(1e10, .5, 5., 3e-2, 0x7fffffff, 1E2, 0xA0, 2^53 + 1)"),
    ("coerce_edges", r#"return S("0x10" * 1, " 2 " * " 3 ", E(function() return "1e" + 1 end), E(function() return "inf" + 0 end), 10 .. "", 1e100 .. "", -0.0 .. "", 2^63 .. "")"#),
    // ── Syntax errors and the lexer's edges, through `loadstring` ──
    ("syntax_errors", r#"local o = {}
        for _, src in ipairs({"x = 1 +", "local function", "x = #t", "x = a % b",
            "return function(...) return ... end", "for x do end", "x = 1..2", "x = \"a\nb\"",
            "x = [[a", "x = 1\1", "f()\n(g)()", "break", "(a) = 1", "if x then\nend end",
            "do\nx = 1", "x = 'a\rb'", "x = 1\ry = +", "x = 1\r\ny = +", "x = 1\n\ry = +",
            "x = [=x", "x = [==[a]=]", "x = 0x", "x = 1e", "x = 3x", "--[==[ c\n]==] y = +",
            "--[[ a [[ b ]] c ]] y = +", "x = [[a\r\nb\rc\n\rd]] y = +"}) do
          local f, e = loadstring(src, "=t")
          table.insert(o, (f and "ok" or e))
        end
        return table.concat(o, " | ")"#),
    ("long_strings", r#"return S([[a [[b]] c]], [==[a ]] [[ ]=] b]==], [[
first newline skipped]], [=[
also]=], [[a
b]])"#),
    // ── Table layout, which `pairs` order shows ──
    ("pairs_constructors", r#"local o = {}
        for n = 1, 20 do
          local parts = {}
          for i = 1, n do table.insert(parts, "k" .. i .. "=" .. i) end
          local t = loadstring("return {" .. table.concat(parts, ",") .. "}")()
          local ks = {} for k in pairs(t) do table.insert(ks, k) end
          table.insert(o, table.concat(ks, " "))
        end
        return table.concat(o, " | ")"#),
    ("pairs_mixed", r#"local function keys(t) local ks = {} for k, v in pairs(t) do table.insert(ks, tostring(k) .. "=" .. tostring(v)) end return table.concat(ks, " ") end
        local a = {1, 2, 3, x = 1, y = 2, [10] = 5, [1.5] = 6, [-1] = 7, z = 3}
        local b = {} for i = 1, 10 do b[i] = i end b[5] = nil b[20] = 20 b.k = 1
        local c = {} for i = 10, 1, -1 do c[i * 3] = i end
        local d = {["a"] = 1, [2] = 2, [true] = 3, b = 4}
        local e = {1, 2, nil, 4, nil, nil, 7}
        return keys(a) .. " | " .. keys(b) .. " | " .. keys(c) .. " | " .. keys(d) .. " | " .. keys(e)"#),
    ("pairs_growth", r#"local o = {}
        local t = {}
        for i = 1, 40 do t["s" .. i] = i if i == 13 or i == 27 then local ks = {} for k in pairs(t) do table.insert(ks, k) end table.insert(o, table.concat(ks, " ")) end end
        for i = 1, 40, 3 do t["s" .. i] = nil end
        for i = 41, 50 do t["s" .. i] = i end
        local ks = {} for k in pairs(t) do table.insert(ks, k) end table.insert(o, table.concat(ks, " "))
        return table.concat(o, " | ")"#),
    ("newindex_layout", r#"local log = {}
        local t = setmetatable({}, {__newindex = function(t, k, v) table.insert(log, k) end})
        for i = 1, 10 do t["k" .. i] = i end
        rawset(t, "z", 1) rawset(t, "y", 2) rawset(t, "x", 3)
        local ks = {} for k in pairs(t) do table.insert(ks, k) end
        local u = setmetatable({}, {__newindex = function(t, k, v) rawset(t, k, v * 10) end})
        u.a = 1 u.b = 2 u.a = 3
        local us = {} for k, v in pairs(u) do table.insert(us, k .. "=" .. v) end
        return table.concat(log, ",") .. " | " .. table.concat(ks, ",") .. " | " .. table.concat(us, ",")"#),
    ("metamethods", r#"local mt = {}
        mt.__concat = function(a, b) return "cat" end
        mt.__eq = function(a, b) return true end
        mt.__lt = function(a, b) return true end
        mt.__le = function(a, b) return false end
        mt.__unm = function(a) return "neg" end
        mt.__call = function(self, x) return "called " .. tostring(x) end
        mt.__index = function(t, k) return "idx " .. k end
        mt.__tostring = function() return "TOSTR" end
        local a, b = setmetatable({}, mt), setmetatable({}, mt)
        local c = setmetatable({}, {__metatable = "locked"})
        return S(a .. "x", "x" .. a, a == b, a < b, a <= b, a > b, -a, a(5), a.foo, tostring(a),
                 getmetatable(c), E(setmetatable, c, {}), rawequal(a, b), E(function() return a < 1 end))"#),
    ("le_fallback", r#"local mt = {__lt = function(a, b) return a.v < b.v end}
        local a, b = setmetatable({v = 1}, mt), setmetatable({v = 2}, mt)
        return S(E(function() return a <= b end), E(function() return b >= a end), a < b)"#),
    ("varargs_arg", r#"local function f(...) return arg.n, arg[1], arg[3] end
        return S(E(f, 1, nil, 3), E(function(a, ...) return a, arg.n end, 9))"#),
    ("pairs_libraries", r#"local o = {}
        for _, lib in ipairs({"string", "table", "math", "bit"}) do
          local ks = {} for k in pairs(_G[lib]) do table.insert(ks, k) end
          table.insert(o, lib .. ": " .. table.concat(ks, " "))
        end
        return table.concat(o, " | ")"#),
    ("pairs_globals", r#"local ks, n = {}, 0
        for k in pairs(_G) do n = n + 1 if n <= 400 then table.insert(ks, tostring(k)) end end
        return n .. ": " .. table.concat(ks, " ")"#),
    ("getn_holes", r#"local function g(t) return table.getn(t) end
        local a = {1, 2, nil, 4}
        local b = {nil, nil, 3}
        local c = {} c[1] = 1 c[2] = 2 c[4] = 4
        local d = {} for i = 1, 20 do d[i] = i end d[7] = nil d[15] = nil
        local e = {} for i = 1, 10 do e[i] = i end for i = 12, 40 do e[i] = i end
        local f = {n = 3}
        local h = {} h[1] = 1 h[3] = 3 h[2] = 2 h[100] = 100
        return S(g(a), g(b), g(c), g(d), g(e), g(f), g(h), g({}), g({nil}), g({1, nil, 3, nil, 5, 6, 7, 8}))"#),
    ("string_metatable", r#"return S(E(function() return ("abc"):upper() end), getmetatable("x") ~= nil, E(function() local s = "x" return s.len end))"#),
    // ── The whole library surface: every global and every library member, with its type ──
    ("global_surface", r#"local o = {} for k, v in pairs(_G) do if type(k) == "string" then table.insert(o, k .. ":" .. type(v)) end end table.sort(o) return table.concat(o, " ")"#),
    ("libraries", r#"local o = {}
        for _, lib in ipairs({"string", "table", "math", "bit", "os", "coroutine", "debug", "io"}) do
          local t = _G[lib]
          if type(t) == "table" then
            local ks = {} for k, v in pairs(t) do table.insert(ks, k .. ":" .. type(v)) end table.sort(ks)
            table.insert(o, lib .. "={" .. table.concat(ks, " ") .. "}")
          else table.insert(o, lib .. "=" .. type(t)) end
        end
        return table.concat(o, " ")"#),
    // ── fix-front: the single-pass front end, 5.1.5's `lparser.c` and `lcode.c` ──
    ("fix-front_setlist_order", r#"local function K(t) local o = {} for k in pairs(t) do table.insert(o, tostring(k)) end return table.concat(o, " ") end
        local function list(n) local o = {} for i = 1, n do o[i] = i end return table.concat(o, ",") end
        local function f() return 1, 2, 3 end
        local big = loadstring("return {" .. list(31) .. ",[1.5]=1,[-1]=2}")()
        local big60 = loadstring("return {x=1," .. list(60) .. ",[1.5]=1,y=2,[-1]=2}")()
        local big120 = loadstring("return {[0.5]=0," .. list(120) .. ",[-2]=1,a=1,b=2}")()
        return S(K({["x"]=4,["y"]=4,["z"]=4,5}), K({0,1,[true]=2,[2]=4,5}), K({0,["x"]=4,["y"]=4,5}),
          K(big), K(big60), K(big120), K({x = 1, f()}), K({f(), x = 1, y = 2, z = 3}), K({[1]=1, [2]=2, 3, f()}))"#),
    ("fix-front_register_reuse", r#"local function L(src) local f, e = loadstring(src) if not f then return e end local ok, r = pcall(f) return tostring(r) end
        local p = {} for i = 1, 250 do p[i] = "x" end
        local q = {} for i = 1, 400 do q[i] = "a*0.25" end
        local locs = {} for i = 1, 190 do locs[i] = "l" .. i end
        return S(L("local x = 1 return " .. table.concat(p, "+")),
          L("local t = {} t.a = t return t" .. string.rep(".a", 300) .. " == t"),
          L("local a = 4 local x = " .. table.concat(q, " + ") .. " return x"),
          L("local " .. table.concat(locs, ",") .. " local t = {} t.x = t return t" .. string.rep(".x", 59) .. " == t"),
          L("local x = 1 return " .. string.rep("(x+", 150) .. "x" .. string.rep(")", 150)),
          L("local x = 1 return " .. string.rep("(x+", 245) .. "x" .. string.rep(")", 245)),
          L("return " .. string.rep("1 .. ", 190) .. "1"))"#),
    ("fix-front_local_limits", r#"local function L(src) local f, e = loadstring(src) if f then return "ok" end return e end
        local function names(p, n) local o = {} for i = 1, n do o[i] = p .. i end return table.concat(o, ",") end
        return S(L("local " .. names("a", 200)), L("local " .. names("a", 201)),
          L("\nfunction f(" .. names("p", 200) .. ") end"), L("function f(" .. names("p", 201) .. ") end"),
          L("function f(" .. names("p", 199) .. ", ...) end"), L("function f(" .. names("p", 200) .. ", ...) end"),
          L("function f(a) local " .. names("b", 199) .. " end"), L("\n\nlocal x = function(a) local " .. names("b", 200) .. " end"),
          L("for " .. names("k", 197) .. " in x do end"), L("for " .. names("k", 198) .. " in x do end"),
          L("local function g() " .. string.rep("do local a end ", 10) .. "end"))"#),
    ("fix-front_upvalue_limits", r#"local function L(src) local f, e = loadstring(src) if f then return "ok" end return e end
        local function names(n) local o = {} for i = 1, n do o[i] = "u" .. i end return table.concat(o, ",") end
        return S(L("local " .. names(60) .. "\nreturn function() return " .. names(60) .. " end"),
          L("local " .. names(61) .. "\nreturn function() return " .. names(61) .. " end"),
          L("local " .. names(61) .. " return function() return function() return " .. names(61) .. " end end"))"#),
    ("fix-front_syntax_levels", r#"local function first_failing(make)
          for n = 1, 400 do
            local f, e = loadstring(make(n))
            if not f then return n .. " " .. e end
          end
          return "none"
        end
        local parens = function(n) return "return " .. string.rep("(", n) .. "1" .. string.rep(")", n) end
        local nots = function(n) return "return " .. string.rep("not ", n) .. "1" end
        local concat = function(n) return "return " .. string.rep("1 .. ", n) .. "1" end
        local blocks = function(n) return string.rep("do ", n) .. string.rep("end ", n) end
        local funcs = function(n) return string.rep("function f() ", n) .. string.rep("end ", n) end
        local tables = function(n) return "return " .. string.rep("{", n) .. string.rep("}", n) end
        local assign = function(n) local o = {} for i = 1, n do o[i] = "a" .. i end return table.concat(o, ",") .. " = 1" end
        local nested = {}
        pcall(function() nested[1] = first_failing(parens) end)
        return S(first_failing(parens), first_failing(nots), first_failing(concat), first_failing(blocks),
          first_failing(funcs), first_failing(tables), first_failing(assign), nested[1])"#),
    ("fix-front_register_limits", r#"local function first_failing(make)
          for n = 200, 260 do
            local f, e = loadstring(make(n))
            if not f then return n .. " " .. e end
          end
          return "none"
        end
        local function args(n) local o = {} for i = 1, n do o[i] = "1" end return table.concat(o, ",") end
        return S(first_failing(function(n) return "f(" .. args(n) .. ")" end),
          first_failing(function(n) return "local a, b, c = 1\nreturn f(" .. args(n) .. ")" end),
          first_failing(function(n) return "return {" .. args(n) .. "}" end),
          first_failing(function(n) return "local t = {" .. args(n) .. "}" end),
          first_failing(function(n) return "x = f(" .. args(n) .. ") $" end))"#),
    ("fix-front_code_limits", r#"local function L(src) local f, e = loadstring(src) if f then return "ok" end return e end
        local function list(n) local o = {} for i = 1, n do o[i] = i end return table.concat(o, ",") end
        local function stats(n) return "local a if a then\n" .. string.rep("a=1 ", n) .. "\nend" end
        return S(L("local t = {" .. list(262143) .. "}"), L("local t = {" .. list(262144) .. "}"),
          L(stats(131070)), L(stats(131071)), L(stats(131072)),
          L("local a while a do\n" .. string.rep("a=1 ", 131072) .. "\nend"))"#),
    ("fix-front_negative_zero", r#"local function R(src) return S(loadstring(src)()) end
        return table.concat({R("return -0, 1/0"), R("local x = 1/0 return -0, x, 1/0"), R("local a = 0 local b = -0 return 1/b"),
          R("return -0, 5, 1/0, 0"), R("return 1/0, -0"),
          R("return 0 * -1, 1/(0 * -1), -0 == 0"), R("local x = -0 return x, 1/x"),
          R("return 2^0.5, 2 - 3, 1/3, 0/0 ~= 0/0, 1e308 * 10")}, " | ")"#),
    ("fix-front_negative_zero_main", "return S(-0, 1/0)"),
    ("fix-front_negative_zero_local", "local x = 1/0 return S(-0, x, 1/0)"),
    ("fix-front_constants_and_folding", r#"local function R(src) return S(loadstring(src)()) end
        return table.concat({R("return 1 + 2 * 3 - 4 / 2, 2^10, -(-3), - - 2, 10 / 0 > 1, 3 .. 4"),
          R("local a = 5 return a + 1 * 2, 2 * a, a / 2, 1 - a, a ^ 2, -a"),
          R("local a = {} return (1 < 2), (2 <= 1), (1 == 1), not nil, not 1, nil and 1, false or nil, 1 and 2 or 3"),
          R("local t = {} t[1], t[2] = 1, 2 local a, b = t[1], t[2] a, b = b, a return a, b"),
          R("local a, b, c = (function() return 1, 2, 3 end)() return a, b, c")}, " | ")"#),
];

/// Every library function the sweep calls, each with every argument set in `SWEEP_ARGS`.
const SWEEP_FNS: &[&str] = &[
    "string.byte",
    "string.char",
    "string.find",
    "string.format",
    "string.gfind",
    "string.gsub",
    "string.len",
    "string.lower",
    "string.rep",
    "string.sub",
    "string.upper",
    "string.match",
    "string.gmatch",
    "string.reverse",
    "table.concat",
    "table.getn",
    "table.setn",
    "table.insert",
    "table.remove",
    "table.sort",
    "table.foreach",
    "table.foreachi",
    "table.maxn",
    "math.abs",
    "math.ceil",
    "math.floor",
    "math.max",
    "math.min",
    "math.mod",
    "math.fmod",
    "math.sqrt",
    "math.exp",
    "math.log",
    "math.log10",
    "math.pow",
    "math.frexp",
    "math.ldexp",
    "math.deg",
    "math.rad",
    "math.sin",
    "math.cos",
    "math.tan",
    "math.asin",
    "math.acos",
    "math.atan",
    "math.atan2",
    "math.modf",
    "math.cosh",
    "math.sinh",
    "math.tanh",
    "tonumber",
    "type",
    "rawequal",
    "rawget",
    "rawset",
    "select",
    "unpack",
    "next",
    "ipairs",
    "pairs",
    "getmetatable",
    "setmetatable",
    "assert",
    "error",
    "pcall",
    "xpcall",
    "getfenv",
    "setfenv",
    "loadstring",
    "strfind",
    "strsub",
    "strlen",
    "strlower",
    "strupper",
    "strrep",
    "format",
    "gsub",
    "tinsert",
    "tremove",
    "getn",
    "foreach",
    "foreachi",
    "sort",
    "abs",
    "ceil",
    "floor",
    "max",
    "min",
    "mod",
    "sqrt",
    "frexp",
    "ldexp",
    "deg",
    "rad",
    "sin",
    "cos",
    "tan",
];

/// The argument sets, built fresh for each call so a mutation stays in it; and the function
/// that runs one name over all of them.
const SWEEP: &str = r#"
local function ARGS() return {
  {}, {nil}, {"abc"}, {"abc", 2}, {"abc", -2}, {"abc", 2, 3}, {"abc", 0, -1}, {"abc", 5, 9},
  {"a.c", "%."}, {"hello", "l+"}, {"hello", "(l)(l)", 1}, {"hello", "l", "L"},
  {"hello", "(h)(e)", "%2%1"}, {"x%sy", "%%"}, {"abc", "[a"}, {"abc", "(", 1}, {1.5}, {-3},
  {0}, {"10"}, {{1, 2, 3}}, {{1, 2, 3}, 2}, {{1, 2, 3}, 2, 9}, {{"a", "b"}, "-", 1, 2},
  {{}, 1}, {2, 3}, {"%5.2f", 3.14159}, {"%d", "x"}, {"%q", "a\"b\n\r\0"}, {true},
  {"abc", "b", 1, true}, {1e300, 1e300}, {-1, 0.5}, {65, 66, 300}, {{3, 1, 2}, "x"},
}
end
local function name(f)
  local t, k = string.find(f, ".", 1, true)
  if t then return _G[string.sub(f, 1, t - 1)][string.sub(f, t + 1)] end
  return _G[f]
end
function SWEEP(fname)
  local f = name(fname)
  if type(f) ~= "function" then return "absent" end
  local o = {}
  for i, a in ipairs(ARGS()) do
    o[i] = E(f, unpack(a))
  end
  return table.concat(o, " | ")
end
"#;

fn run(src: &str) -> String {
    let s = UiScript::new().expect("a script host");
    match s.lua().load(src).set_name("=t").eval::<String>() {
        Ok(v) => v,
        Err(e) => format!("<raised> {e}"),
    }
}

const GOLDEN: &str = "tests/vm_parity.golden";

/// `escape_default` undone: the golden file's one-line answers back to text.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = it.by_ref().skip(1).take_while(|&c| c != '}').collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
            }
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

/// The C Lua build hands Lua an error from a Rust callback through mlua's wrapper:
/// `runtime error: ` before the message, and a Rust-side `stack traceback:` block after it.
/// Neither Lua nor the 1.12 client writes either, and benilla's own VM raises the message alone,
/// so the wrapper is taken out of a golden answer before it is compared; the message must match.
fn without_mlua_wrapper(s: &str) -> String {
    let s = s.replace("runtime error: ", "");
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("\nstack traceback:") {
        out.push_str(&rest[..i]);
        rest = &rest[i + "\nstack traceback:".len()..];
        // The block's lines each open with a tab; one newline after it goes with it.
        while let Some(r) = rest.strip_prefix("\n\t") {
            rest = r.find('\n').map_or("", |j| &r[j..]);
        }
        rest = rest.strip_prefix('\n').unwrap_or(rest);
        rest = rest.strip_prefix("stack traceback:").map_or(rest, |r| {
            // A second block straight after the first, as a rethrown error carries.
            let mut r = r;
            while let Some(x) = r.strip_prefix("\n\t") {
                r = x.find('\n').map_or("", |j| &x[j..]);
            }
            r.strip_prefix('\n').unwrap_or(r)
        });
    }
    out.push_str(rest);
    out
}

/// `name\tanswer`, one case a line, the answer escaped onto one line.
fn render(answers: &[(&str, String)]) -> String {
    answers
        .iter()
        .map(|(n, a)| format!("{n}\t{}\n", a.escape_default()))
        .collect()
}

#[test]
fn every_case_answers_as_the_c_lua_build_recorded() {
    let sweep: Vec<(String, String)> = SWEEP_FNS
        .iter()
        .map(|f| (format!("sweep {f}"), format!("return SWEEP({f:?})")))
        .collect();
    let cases: Vec<(&str, String)> = CASES
        .iter()
        .map(|(n, b)| (*n, b.to_string()))
        .chain(
            sweep
                .iter()
                .map(|(n, b)| (n.as_str(), format!("{SWEEP}\n{b}"))),
        )
        .collect();
    let answers: Vec<(&str, String)> = cases
        .iter()
        .map(|(name, body)| (*name, run(&format!("{PRELUDE}\n{body}"))))
        .collect();
    let mut names: Vec<&str> = cases.iter().map(|c| c.0).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), cases.len(), "case names are unique");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("BENILLA_BLESS").is_some() {
        if cfg!(feature = "benilla-vm") {
            panic!("the golden answers come from the C Lua build");
        }
        std::fs::write(&path, render(&answers)).expect("write the golden file");
        return;
    }
    let golden = std::fs::read_to_string(&path).expect("the golden file");
    let want: std::collections::HashMap<&str, &str> =
        golden.lines().filter_map(|l| l.split_once('\t')).collect();
    let mut failures = Vec::new();
    for (name, got) in &answers {
        let got = got.escape_default().to_string();
        match want.get(name) {
            Some(w) if *w == got => {}
            Some(w)
                if without_mlua_wrapper(&unescape(w))
                    .escape_default()
                    .to_string()
                    == got => {}
            Some(w) => {
                let w = without_mlua_wrapper(&unescape(w))
                    .escape_default()
                    .to_string();
                failures.push(format!("{name}\n    got:  {got}\n    want: {w}"))
            }
            None => failures.push(format!(
                "{name}: no golden answer; bless on the C Lua build"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
