(async ()=>{
    (function() {
        const a = document.createElement("link").relList;
        if (a && a.supports && a.supports("modulepreload")) return;
        for (const u of document.querySelectorAll('link[rel="modulepreload"]'))r(u);
        new MutationObserver((u)=>{
            for (const d of u)if (d.type === "childList") for (const f of d.addedNodes)f.tagName === "LINK" && f.rel === "modulepreload" && r(f);
        }).observe(document, {
            childList: !0,
            subtree: !0
        });
        function l(u) {
            const d = {};
            return u.integrity && (d.integrity = u.integrity), u.referrerPolicy && (d.referrerPolicy = u.referrerPolicy), u.crossOrigin === "use-credentials" ? d.credentials = "include" : u.crossOrigin === "anonymous" ? d.credentials = "omit" : d.credentials = "same-origin", d;
        }
        function r(u) {
            if (u.ep) return;
            u.ep = !0;
            const d = l(u);
            fetch(u.href, d);
        }
    })();
    function f1(n) {
        return n && n.__esModule && Object.prototype.hasOwnProperty.call(n, "default") ? n.default : n;
    }
    var Ec = {
        exports: {}
    }, Rl = {};
    var Ag;
    function d1() {
        if (Ag) return Rl;
        Ag = 1;
        var n = Symbol.for("react.transitional.element"), a = Symbol.for("react.fragment");
        function l(r, u, d) {
            var f = null;
            if (d !== void 0 && (f = "" + d), u.key !== void 0 && (f = "" + u.key), "key" in u) {
                d = {};
                for(var h in u)h !== "key" && (d[h] = u[h]);
            } else d = u;
            return u = d.ref, {
                $$typeof: n,
                type: r,
                key: f,
                ref: u !== void 0 ? u : null,
                props: d
            };
        }
        return Rl.Fragment = a, Rl.jsx = l, Rl.jsxs = l, Rl;
    }
    var Cg;
    function h1() {
        return Cg || (Cg = 1, Ec.exports = d1()), Ec.exports;
    }
    var S = h1(), Ac = {
        exports: {}
    }, rt = {};
    var wg;
    function m1() {
        if (wg) return rt;
        wg = 1;
        var n = Symbol.for("react.transitional.element"), a = Symbol.for("react.portal"), l = Symbol.for("react.fragment"), r = Symbol.for("react.strict_mode"), u = Symbol.for("react.profiler"), d = Symbol.for("react.consumer"), f = Symbol.for("react.context"), h = Symbol.for("react.forward_ref"), m = Symbol.for("react.suspense"), p = Symbol.for("react.memo"), y = Symbol.for("react.lazy"), v = Symbol.for("react.activity"), x = Symbol.iterator;
        function A(w) {
            return w === null || typeof w != "object" ? null : (w = x && w[x] || w["@@iterator"], typeof w == "function" ? w : null);
        }
        var E = {
            isMounted: function() {
                return !1;
            },
            enqueueForceUpdate: function() {},
            enqueueReplaceState: function() {},
            enqueueSetState: function() {}
        }, M = Object.assign, R = {};
        function z(w, Y, J) {
            this.props = w, this.context = Y, this.refs = R, this.updater = J || E;
        }
        z.prototype.isReactComponent = {}, z.prototype.setState = function(w, Y) {
            if (typeof w != "object" && typeof w != "function" && w != null) throw Error("takes an object of state variables to update or a function which returns an object of state variables.");
            this.updater.enqueueSetState(this, w, Y, "setState");
        }, z.prototype.forceUpdate = function(w) {
            this.updater.enqueueForceUpdate(this, w, "forceUpdate");
        };
        function B() {}
        B.prototype = z.prototype;
        function V(w, Y, J) {
            this.props = w, this.context = Y, this.refs = R, this.updater = J || E;
        }
        var P = V.prototype = new B;
        P.constructor = V, M(P, z.prototype), P.isPureReactComponent = !0;
        var U = Array.isArray;
        function X() {}
        var H = {
            H: null,
            A: null,
            T: null,
            S: null
        }, Z = Object.prototype.hasOwnProperty;
        function Q(w, Y, J) {
            var W = J.ref;
            return {
                $$typeof: n,
                type: w,
                key: Y,
                ref: W !== void 0 ? W : null,
                props: J
            };
        }
        function it(w, Y) {
            return Q(w.type, Y, w.props);
        }
        function bt(w) {
            return typeof w == "object" && w !== null && w.$$typeof === n;
        }
        function gt(w) {
            var Y = {
                "=": "=0",
                ":": "=2"
            };
            return "$" + w.replace(/[=:]/g, function(J) {
                return Y[J];
            });
        }
        var Nt = /\/+/g;
        function ee(w, Y) {
            return typeof w == "object" && w !== null && w.key != null ? gt("" + w.key) : Y.toString(36);
        }
        function Vt(w) {
            switch(w.status){
                case "fulfilled":
                    return w.value;
                case "rejected":
                    throw w.reason;
                default:
                    switch(typeof w.status == "string" ? w.then(X, X) : (w.status = "pending", w.then(function(Y) {
                        w.status === "pending" && (w.status = "fulfilled", w.value = Y);
                    }, function(Y) {
                        w.status === "pending" && (w.status = "rejected", w.reason = Y);
                    })), w.status){
                        case "fulfilled":
                            return w.value;
                        case "rejected":
                            throw w.reason;
                    }
            }
            throw w;
        }
        function G(w, Y, J, W, lt) {
            var ot = typeof w;
            (ot === "undefined" || ot === "boolean") && (w = null);
            var yt = !1;
            if (w === null) yt = !0;
            else switch(ot){
                case "bigint":
                case "string":
                case "number":
                    yt = !0;
                    break;
                case "object":
                    switch(w.$$typeof){
                        case n:
                        case a:
                            yt = !0;
                            break;
                        case y:
                            return yt = w._init, G(yt(w._payload), Y, J, W, lt);
                    }
            }
            if (yt) return lt = lt(w), yt = W === "" ? "." + ee(w, 0) : W, U(lt) ? (J = "", yt != null && (J = yt.replace(Nt, "$&/") + "/"), G(lt, Y, J, "", function(Li) {
                return Li;
            })) : lt != null && (bt(lt) && (lt = it(lt, J + (lt.key == null || w && w.key === lt.key ? "" : ("" + lt.key).replace(Nt, "$&/") + "/") + yt)), Y.push(lt)), 1;
            yt = 0;
            var Xt = W === "" ? "." : W + ":";
            if (U(w)) for(var Dt = 0; Dt < w.length; Dt++)W = w[Dt], ot = Xt + ee(W, Dt), yt += G(W, Y, J, ot, lt);
            else if (Dt = A(w), typeof Dt == "function") for(w = Dt.call(w), Dt = 0; !(W = w.next()).done;)W = W.value, ot = Xt + ee(W, Dt++), yt += G(W, Y, J, ot, lt);
            else if (ot === "object") {
                if (typeof w.then == "function") return G(Vt(w), Y, J, W, lt);
                throw Y = String(w), Error("Objects are not valid as a React child (found: " + (Y === "[object Object]" ? "object with keys {" + Object.keys(w).join(", ") + "}" : Y) + "). If you meant to render a collection of children, use an array instead.");
            }
            return yt;
        }
        function F(w, Y, J) {
            if (w == null) return w;
            var W = [], lt = 0;
            return G(w, W, "", "", function(ot) {
                return Y.call(J, ot, lt++);
            }), W;
        }
        function $(w) {
            if (w._status === -1) {
                var Y = w._result;
                Y = Y(), Y.then(function(J) {
                    (w._status === 0 || w._status === -1) && (w._status = 1, w._result = J);
                }, function(J) {
                    (w._status === 0 || w._status === -1) && (w._status = 2, w._result = J);
                }), w._status === -1 && (w._status = 0, w._result = Y);
            }
            if (w._status === 1) return w._result.default;
            throw w._result;
        }
        var st = typeof reportError == "function" ? reportError : function(w) {
            if (typeof window == "object" && typeof window.ErrorEvent == "function") {
                var Y = new window.ErrorEvent("error", {
                    bubbles: !0,
                    cancelable: !0,
                    message: typeof w == "object" && w !== null && typeof w.message == "string" ? String(w.message) : String(w),
                    error: w
                });
                if (!window.dispatchEvent(Y)) return;
            } else if (typeof process == "object" && typeof process.emit == "function") {
                process.emit("uncaughtException", w);
                return;
            }
            console.error(w);
        }, ft = {
            map: F,
            forEach: function(w, Y, J) {
                F(w, function() {
                    Y.apply(this, arguments);
                }, J);
            },
            count: function(w) {
                var Y = 0;
                return F(w, function() {
                    Y++;
                }), Y;
            },
            toArray: function(w) {
                return F(w, function(Y) {
                    return Y;
                }) || [];
            },
            only: function(w) {
                if (!bt(w)) throw Error("React.Children.only expected to receive a single React element child.");
                return w;
            }
        };
        return rt.Activity = v, rt.Children = ft, rt.Component = z, rt.Fragment = l, rt.Profiler = u, rt.PureComponent = V, rt.StrictMode = r, rt.Suspense = m, rt.__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE = H, rt.__COMPILER_RUNTIME = {
            __proto__: null,
            c: function(w) {
                return H.H.useMemoCache(w);
            }
        }, rt.cache = function(w) {
            return function() {
                return w.apply(null, arguments);
            };
        }, rt.cacheSignal = function() {
            return null;
        }, rt.cloneElement = function(w, Y, J) {
            if (w == null) throw Error("The argument must be a React element, but you passed " + w + ".");
            var W = M({}, w.props), lt = w.key;
            if (Y != null) for(ot in Y.key !== void 0 && (lt = "" + Y.key), Y)!Z.call(Y, ot) || ot === "key" || ot === "__self" || ot === "__source" || ot === "ref" && Y.ref === void 0 || (W[ot] = Y[ot]);
            var ot = arguments.length - 2;
            if (ot === 1) W.children = J;
            else if (1 < ot) {
                for(var yt = Array(ot), Xt = 0; Xt < ot; Xt++)yt[Xt] = arguments[Xt + 2];
                W.children = yt;
            }
            return Q(w.type, lt, W);
        }, rt.createContext = function(w) {
            return w = {
                $$typeof: f,
                _currentValue: w,
                _currentValue2: w,
                _threadCount: 0,
                Provider: null,
                Consumer: null
            }, w.Provider = w, w.Consumer = {
                $$typeof: d,
                _context: w
            }, w;
        }, rt.createElement = function(w, Y, J) {
            var W, lt = {}, ot = null;
            if (Y != null) for(W in Y.key !== void 0 && (ot = "" + Y.key), Y)Z.call(Y, W) && W !== "key" && W !== "__self" && W !== "__source" && (lt[W] = Y[W]);
            var yt = arguments.length - 2;
            if (yt === 1) lt.children = J;
            else if (1 < yt) {
                for(var Xt = Array(yt), Dt = 0; Dt < yt; Dt++)Xt[Dt] = arguments[Dt + 2];
                lt.children = Xt;
            }
            if (w && w.defaultProps) for(W in yt = w.defaultProps, yt)lt[W] === void 0 && (lt[W] = yt[W]);
            return Q(w, ot, lt);
        }, rt.createRef = function() {
            return {
                current: null
            };
        }, rt.forwardRef = function(w) {
            return {
                $$typeof: h,
                render: w
            };
        }, rt.isValidElement = bt, rt.lazy = function(w) {
            return {
                $$typeof: y,
                _payload: {
                    _status: -1,
                    _result: w
                },
                _init: $
            };
        }, rt.memo = function(w, Y) {
            return {
                $$typeof: p,
                type: w,
                compare: Y === void 0 ? null : Y
            };
        }, rt.startTransition = function(w) {
            var Y = H.T, J = {};
            H.T = J;
            try {
                var W = w(), lt = H.S;
                lt !== null && lt(J, W), typeof W == "object" && W !== null && typeof W.then == "function" && W.then(X, st);
            } catch (ot) {
                st(ot);
            } finally{
                Y !== null && J.types !== null && (Y.types = J.types), H.T = Y;
            }
        }, rt.unstable_useCacheRefresh = function() {
            return H.H.useCacheRefresh();
        }, rt.use = function(w) {
            return H.H.use(w);
        }, rt.useActionState = function(w, Y, J) {
            return H.H.useActionState(w, Y, J);
        }, rt.useCallback = function(w, Y) {
            return H.H.useCallback(w, Y);
        }, rt.useContext = function(w) {
            return H.H.useContext(w);
        }, rt.useDebugValue = function() {}, rt.useDeferredValue = function(w, Y) {
            return H.H.useDeferredValue(w, Y);
        }, rt.useEffect = function(w, Y) {
            return H.H.useEffect(w, Y);
        }, rt.useEffectEvent = function(w) {
            return H.H.useEffectEvent(w);
        }, rt.useId = function() {
            return H.H.useId();
        }, rt.useImperativeHandle = function(w, Y, J) {
            return H.H.useImperativeHandle(w, Y, J);
        }, rt.useInsertionEffect = function(w, Y) {
            return H.H.useInsertionEffect(w, Y);
        }, rt.useLayoutEffect = function(w, Y) {
            return H.H.useLayoutEffect(w, Y);
        }, rt.useMemo = function(w, Y) {
            return H.H.useMemo(w, Y);
        }, rt.useOptimistic = function(w, Y) {
            return H.H.useOptimistic(w, Y);
        }, rt.useReducer = function(w, Y, J) {
            return H.H.useReducer(w, Y, J);
        }, rt.useRef = function(w) {
            return H.H.useRef(w);
        }, rt.useState = function(w) {
            return H.H.useState(w);
        }, rt.useSyncExternalStore = function(w, Y, J) {
            return H.H.useSyncExternalStore(w, Y, J);
        }, rt.useTransition = function() {
            return H.H.useTransition();
        }, rt.version = "19.2.4", rt;
    }
    var _g;
    function Of() {
        return _g || (_g = 1, Ac.exports = m1()), Ac.exports;
    }
    var T = Of();
    const Sr = f1(T);
    var Cc = {
        exports: {}
    }, Ml = {}, wc = {
        exports: {}
    }, _c = {};
    var Rg;
    function p1() {
        return Rg || (Rg = 1, (function(n) {
            function a(G, F) {
                var $ = G.length;
                G.push(F);
                t: for(; 0 < $;){
                    var st = $ - 1 >>> 1, ft = G[st];
                    if (0 < u(ft, F)) G[st] = F, G[$] = ft, $ = st;
                    else break t;
                }
            }
            function l(G) {
                return G.length === 0 ? null : G[0];
            }
            function r(G) {
                if (G.length === 0) return null;
                var F = G[0], $ = G.pop();
                if ($ !== F) {
                    G[0] = $;
                    t: for(var st = 0, ft = G.length, w = ft >>> 1; st < w;){
                        var Y = 2 * (st + 1) - 1, J = G[Y], W = Y + 1, lt = G[W];
                        if (0 > u(J, $)) W < ft && 0 > u(lt, J) ? (G[st] = lt, G[W] = $, st = W) : (G[st] = J, G[Y] = $, st = Y);
                        else if (W < ft && 0 > u(lt, $)) G[st] = lt, G[W] = $, st = W;
                        else break t;
                    }
                }
                return F;
            }
            function u(G, F) {
                var $ = G.sortIndex - F.sortIndex;
                return $ !== 0 ? $ : G.id - F.id;
            }
            if (n.unstable_now = void 0, typeof performance == "object" && typeof performance.now == "function") {
                var d = performance;
                n.unstable_now = function() {
                    return d.now();
                };
            } else {
                var f = Date, h = f.now();
                n.unstable_now = function() {
                    return f.now() - h;
                };
            }
            var m = [], p = [], y = 1, v = null, x = 3, A = !1, E = !1, M = !1, R = !1, z = typeof setTimeout == "function" ? setTimeout : null, B = typeof clearTimeout == "function" ? clearTimeout : null, V = typeof setImmediate < "u" ? setImmediate : null;
            function P(G) {
                for(var F = l(p); F !== null;){
                    if (F.callback === null) r(p);
                    else if (F.startTime <= G) r(p), F.sortIndex = F.expirationTime, a(m, F);
                    else break;
                    F = l(p);
                }
            }
            function U(G) {
                if (M = !1, P(G), !E) if (l(m) !== null) E = !0, X || (X = !0, gt());
                else {
                    var F = l(p);
                    F !== null && Vt(U, F.startTime - G);
                }
            }
            var X = !1, H = -1, Z = 5, Q = -1;
            function it() {
                return R ? !0 : !(n.unstable_now() - Q < Z);
            }
            function bt() {
                if (R = !1, X) {
                    var G = n.unstable_now();
                    Q = G;
                    var F = !0;
                    try {
                        t: {
                            E = !1, M && (M = !1, B(H), H = -1), A = !0;
                            var $ = x;
                            try {
                                e: {
                                    for(P(G), v = l(m); v !== null && !(v.expirationTime > G && it());){
                                        var st = v.callback;
                                        if (typeof st == "function") {
                                            v.callback = null, x = v.priorityLevel;
                                            var ft = st(v.expirationTime <= G);
                                            if (G = n.unstable_now(), typeof ft == "function") {
                                                v.callback = ft, P(G), F = !0;
                                                break e;
                                            }
                                            v === l(m) && r(m), P(G);
                                        } else r(m);
                                        v = l(m);
                                    }
                                    if (v !== null) F = !0;
                                    else {
                                        var w = l(p);
                                        w !== null && Vt(U, w.startTime - G), F = !1;
                                    }
                                }
                                break t;
                            } finally{
                                v = null, x = $, A = !1;
                            }
                            F = void 0;
                        }
                    } finally{
                        F ? gt() : X = !1;
                    }
                }
            }
            var gt;
            if (typeof V == "function") gt = function() {
                V(bt);
            };
            else if (typeof MessageChannel < "u") {
                var Nt = new MessageChannel, ee = Nt.port2;
                Nt.port1.onmessage = bt, gt = function() {
                    ee.postMessage(null);
                };
            } else gt = function() {
                z(bt, 0);
            };
            function Vt(G, F) {
                H = z(function() {
                    G(n.unstable_now());
                }, F);
            }
            n.unstable_IdlePriority = 5, n.unstable_ImmediatePriority = 1, n.unstable_LowPriority = 4, n.unstable_NormalPriority = 3, n.unstable_Profiling = null, n.unstable_UserBlockingPriority = 2, n.unstable_cancelCallback = function(G) {
                G.callback = null;
            }, n.unstable_forceFrameRate = function(G) {
                0 > G || 125 < G ? console.error("forceFrameRate takes a positive int between 0 and 125, forcing frame rates higher than 125 fps is not supported") : Z = 0 < G ? Math.floor(1e3 / G) : 5;
            }, n.unstable_getCurrentPriorityLevel = function() {
                return x;
            }, n.unstable_next = function(G) {
                switch(x){
                    case 1:
                    case 2:
                    case 3:
                        var F = 3;
                        break;
                    default:
                        F = x;
                }
                var $ = x;
                x = F;
                try {
                    return G();
                } finally{
                    x = $;
                }
            }, n.unstable_requestPaint = function() {
                R = !0;
            }, n.unstable_runWithPriority = function(G, F) {
                switch(G){
                    case 1:
                    case 2:
                    case 3:
                    case 4:
                    case 5:
                        break;
                    default:
                        G = 3;
                }
                var $ = x;
                x = G;
                try {
                    return F();
                } finally{
                    x = $;
                }
            }, n.unstable_scheduleCallback = function(G, F, $) {
                var st = n.unstable_now();
                switch(typeof $ == "object" && $ !== null ? ($ = $.delay, $ = typeof $ == "number" && 0 < $ ? st + $ : st) : $ = st, G){
                    case 1:
                        var ft = -1;
                        break;
                    case 2:
                        ft = 250;
                        break;
                    case 5:
                        ft = 1073741823;
                        break;
                    case 4:
                        ft = 1e4;
                        break;
                    default:
                        ft = 5e3;
                }
                return ft = $ + ft, G = {
                    id: y++,
                    callback: F,
                    priorityLevel: G,
                    startTime: $,
                    expirationTime: ft,
                    sortIndex: -1
                }, $ > st ? (G.sortIndex = $, a(p, G), l(m) === null && G === l(p) && (M ? (B(H), H = -1) : M = !0, Vt(U, $ - st))) : (G.sortIndex = ft, a(m, G), E || A || (E = !0, X || (X = !0, gt()))), G;
            }, n.unstable_shouldYield = it, n.unstable_wrapCallback = function(G) {
                var F = x;
                return function() {
                    var $ = x;
                    x = F;
                    try {
                        return G.apply(this, arguments);
                    } finally{
                        x = $;
                    }
                };
            };
        })(_c)), _c;
    }
    var Mg;
    function g1() {
        return Mg || (Mg = 1, wc.exports = p1()), wc.exports;
    }
    var Rc = {
        exports: {}
    }, fe = {};
    var Dg;
    function y1() {
        if (Dg) return fe;
        Dg = 1;
        var n = Of();
        function a(m) {
            var p = "https://react.dev/errors/" + m;
            if (1 < arguments.length) {
                p += "?args[]=" + encodeURIComponent(arguments[1]);
                for(var y = 2; y < arguments.length; y++)p += "&args[]=" + encodeURIComponent(arguments[y]);
            }
            return "Minified React error #" + m + "; visit " + p + " for the full message or use the non-minified dev environment for full errors and additional helpful warnings.";
        }
        function l() {}
        var r = {
            d: {
                f: l,
                r: function() {
                    throw Error(a(522));
                },
                D: l,
                C: l,
                L: l,
                m: l,
                X: l,
                S: l,
                M: l
            },
            p: 0,
            findDOMNode: null
        }, u = Symbol.for("react.portal");
        function d(m, p, y) {
            var v = 3 < arguments.length && arguments[3] !== void 0 ? arguments[3] : null;
            return {
                $$typeof: u,
                key: v == null ? null : "" + v,
                children: m,
                containerInfo: p,
                implementation: y
            };
        }
        var f = n.__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE;
        function h(m, p) {
            if (m === "font") return "";
            if (typeof p == "string") return p === "use-credentials" ? p : "";
        }
        return fe.__DOM_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE = r, fe.createPortal = function(m, p) {
            var y = 2 < arguments.length && arguments[2] !== void 0 ? arguments[2] : null;
            if (!p || p.nodeType !== 1 && p.nodeType !== 9 && p.nodeType !== 11) throw Error(a(299));
            return d(m, p, null, y);
        }, fe.flushSync = function(m) {
            var p = f.T, y = r.p;
            try {
                if (f.T = null, r.p = 2, m) return m();
            } finally{
                f.T = p, r.p = y, r.d.f();
            }
        }, fe.preconnect = function(m, p) {
            typeof m == "string" && (p ? (p = p.crossOrigin, p = typeof p == "string" ? p === "use-credentials" ? p : "" : void 0) : p = null, r.d.C(m, p));
        }, fe.prefetchDNS = function(m) {
            typeof m == "string" && r.d.D(m);
        }, fe.preinit = function(m, p) {
            if (typeof m == "string" && p && typeof p.as == "string") {
                var y = p.as, v = h(y, p.crossOrigin), x = typeof p.integrity == "string" ? p.integrity : void 0, A = typeof p.fetchPriority == "string" ? p.fetchPriority : void 0;
                y === "style" ? r.d.S(m, typeof p.precedence == "string" ? p.precedence : void 0, {
                    crossOrigin: v,
                    integrity: x,
                    fetchPriority: A
                }) : y === "script" && r.d.X(m, {
                    crossOrigin: v,
                    integrity: x,
                    fetchPriority: A,
                    nonce: typeof p.nonce == "string" ? p.nonce : void 0
                });
            }
        }, fe.preinitModule = function(m, p) {
            if (typeof m == "string") if (typeof p == "object" && p !== null) {
                if (p.as == null || p.as === "script") {
                    var y = h(p.as, p.crossOrigin);
                    r.d.M(m, {
                        crossOrigin: y,
                        integrity: typeof p.integrity == "string" ? p.integrity : void 0,
                        nonce: typeof p.nonce == "string" ? p.nonce : void 0
                    });
                }
            } else p == null && r.d.M(m);
        }, fe.preload = function(m, p) {
            if (typeof m == "string" && typeof p == "object" && p !== null && typeof p.as == "string") {
                var y = p.as, v = h(y, p.crossOrigin);
                r.d.L(m, y, {
                    crossOrigin: v,
                    integrity: typeof p.integrity == "string" ? p.integrity : void 0,
                    nonce: typeof p.nonce == "string" ? p.nonce : void 0,
                    type: typeof p.type == "string" ? p.type : void 0,
                    fetchPriority: typeof p.fetchPriority == "string" ? p.fetchPriority : void 0,
                    referrerPolicy: typeof p.referrerPolicy == "string" ? p.referrerPolicy : void 0,
                    imageSrcSet: typeof p.imageSrcSet == "string" ? p.imageSrcSet : void 0,
                    imageSizes: typeof p.imageSizes == "string" ? p.imageSizes : void 0,
                    media: typeof p.media == "string" ? p.media : void 0
                });
            }
        }, fe.preloadModule = function(m, p) {
            if (typeof m == "string") if (p) {
                var y = h(p.as, p.crossOrigin);
                r.d.m(m, {
                    as: typeof p.as == "string" && p.as !== "script" ? p.as : void 0,
                    crossOrigin: y,
                    integrity: typeof p.integrity == "string" ? p.integrity : void 0
                });
            } else r.d.m(m);
        }, fe.requestFormReset = function(m) {
            r.d.r(m);
        }, fe.unstable_batchedUpdates = function(m, p) {
            return m(p);
        }, fe.useFormState = function(m, p, y) {
            return f.H.useFormState(m, p, y);
        }, fe.useFormStatus = function() {
            return f.H.useHostTransitionStatus();
        }, fe.version = "19.2.4", fe;
    }
    var jg;
    function v1() {
        if (jg) return Rc.exports;
        jg = 1;
        function n() {
            if (!(typeof __REACT_DEVTOOLS_GLOBAL_HOOK__ > "u" || typeof __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE != "function")) try {
                __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE(n);
            } catch (a) {
                console.error(a);
            }
        }
        return n(), Rc.exports = y1(), Rc.exports;
    }
    var Og;
    function b1() {
        if (Og) return Ml;
        Og = 1;
        var n = g1(), a = Of(), l = v1();
        function r(t) {
            var e = "https://react.dev/errors/" + t;
            if (1 < arguments.length) {
                e += "?args[]=" + encodeURIComponent(arguments[1]);
                for(var i = 2; i < arguments.length; i++)e += "&args[]=" + encodeURIComponent(arguments[i]);
            }
            return "Minified React error #" + t + "; visit " + e + " for the full message or use the non-minified dev environment for full errors and additional helpful warnings.";
        }
        function u(t) {
            return !(!t || t.nodeType !== 1 && t.nodeType !== 9 && t.nodeType !== 11);
        }
        function d(t) {
            var e = t, i = t;
            if (t.alternate) for(; e.return;)e = e.return;
            else {
                t = e;
                do e = t, (e.flags & 4098) !== 0 && (i = e.return), t = e.return;
                while (t);
            }
            return e.tag === 3 ? i : null;
        }
        function f(t) {
            if (t.tag === 13) {
                var e = t.memoizedState;
                if (e === null && (t = t.alternate, t !== null && (e = t.memoizedState)), e !== null) return e.dehydrated;
            }
            return null;
        }
        function h(t) {
            if (t.tag === 31) {
                var e = t.memoizedState;
                if (e === null && (t = t.alternate, t !== null && (e = t.memoizedState)), e !== null) return e.dehydrated;
            }
            return null;
        }
        function m(t) {
            if (d(t) !== t) throw Error(r(188));
        }
        function p(t) {
            var e = t.alternate;
            if (!e) {
                if (e = d(t), e === null) throw Error(r(188));
                return e !== t ? null : t;
            }
            for(var i = t, s = e;;){
                var o = i.return;
                if (o === null) break;
                var c = o.alternate;
                if (c === null) {
                    if (s = o.return, s !== null) {
                        i = s;
                        continue;
                    }
                    break;
                }
                if (o.child === c.child) {
                    for(c = o.child; c;){
                        if (c === i) return m(o), t;
                        if (c === s) return m(o), e;
                        c = c.sibling;
                    }
                    throw Error(r(188));
                }
                if (i.return !== s.return) i = o, s = c;
                else {
                    for(var g = !1, b = o.child; b;){
                        if (b === i) {
                            g = !0, i = o, s = c;
                            break;
                        }
                        if (b === s) {
                            g = !0, s = o, i = c;
                            break;
                        }
                        b = b.sibling;
                    }
                    if (!g) {
                        for(b = c.child; b;){
                            if (b === i) {
                                g = !0, i = c, s = o;
                                break;
                            }
                            if (b === s) {
                                g = !0, s = c, i = o;
                                break;
                            }
                            b = b.sibling;
                        }
                        if (!g) throw Error(r(189));
                    }
                }
                if (i.alternate !== s) throw Error(r(190));
            }
            if (i.tag !== 3) throw Error(r(188));
            return i.stateNode.current === i ? t : e;
        }
        function y(t) {
            var e = t.tag;
            if (e === 5 || e === 26 || e === 27 || e === 6) return t;
            for(t = t.child; t !== null;){
                if (e = y(t), e !== null) return e;
                t = t.sibling;
            }
            return null;
        }
        var v = Object.assign, x = Symbol.for("react.element"), A = Symbol.for("react.transitional.element"), E = Symbol.for("react.portal"), M = Symbol.for("react.fragment"), R = Symbol.for("react.strict_mode"), z = Symbol.for("react.profiler"), B = Symbol.for("react.consumer"), V = Symbol.for("react.context"), P = Symbol.for("react.forward_ref"), U = Symbol.for("react.suspense"), X = Symbol.for("react.suspense_list"), H = Symbol.for("react.memo"), Z = Symbol.for("react.lazy"), Q = Symbol.for("react.activity"), it = Symbol.for("react.memo_cache_sentinel"), bt = Symbol.iterator;
        function gt(t) {
            return t === null || typeof t != "object" ? null : (t = bt && t[bt] || t["@@iterator"], typeof t == "function" ? t : null);
        }
        var Nt = Symbol.for("react.client.reference");
        function ee(t) {
            if (t == null) return null;
            if (typeof t == "function") return t.$$typeof === Nt ? null : t.displayName || t.name || null;
            if (typeof t == "string") return t;
            switch(t){
                case M:
                    return "Fragment";
                case z:
                    return "Profiler";
                case R:
                    return "StrictMode";
                case U:
                    return "Suspense";
                case X:
                    return "SuspenseList";
                case Q:
                    return "Activity";
            }
            if (typeof t == "object") switch(t.$$typeof){
                case E:
                    return "Portal";
                case V:
                    return t.displayName || "Context";
                case B:
                    return (t._context.displayName || "Context") + ".Consumer";
                case P:
                    var e = t.render;
                    return t = t.displayName, t || (t = e.displayName || e.name || "", t = t !== "" ? "ForwardRef(" + t + ")" : "ForwardRef"), t;
                case H:
                    return e = t.displayName || null, e !== null ? e : ee(t.type) || "Memo";
                case Z:
                    e = t._payload, t = t._init;
                    try {
                        return ee(t(e));
                    } catch  {}
            }
            return null;
        }
        var Vt = Array.isArray, G = a.__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE, F = l.__DOM_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE, $ = {
            pending: !1,
            data: null,
            method: null,
            action: null
        }, st = [], ft = -1;
        function w(t) {
            return {
                current: t
            };
        }
        function Y(t) {
            0 > ft || (t.current = st[ft], st[ft] = null, ft--);
        }
        function J(t, e) {
            ft++, st[ft] = t.current, t.current = e;
        }
        var W = w(null), lt = w(null), ot = w(null), yt = w(null);
        function Xt(t, e) {
            switch(J(ot, e), J(lt, t), J(W, null), e.nodeType){
                case 9:
                case 11:
                    t = (t = e.documentElement) && (t = t.namespaceURI) ? Zp(t) : 0;
                    break;
                default:
                    if (t = e.tagName, e = e.namespaceURI) e = Zp(e), t = Qp(e, t);
                    else switch(t){
                        case "svg":
                            t = 1;
                            break;
                        case "math":
                            t = 2;
                            break;
                        default:
                            t = 0;
                    }
            }
            Y(W), J(W, t);
        }
        function Dt() {
            Y(W), Y(lt), Y(ot);
        }
        function Li(t) {
            t.memoizedState !== null && J(yt, t);
            var e = W.current, i = Qp(e, t.type);
            e !== i && (J(lt, t), J(W, i));
        }
        function as(t) {
            lt.current === t && (Y(W), Y(lt)), yt.current === t && (Y(yt), Al._currentValue = $);
        }
        var io, Ed;
        function fa(t) {
            if (io === void 0) try {
                throw Error();
            } catch (i) {
                var e = i.stack.trim().match(/\n( *(at )?)/);
                io = e && e[1] || "", Ed = -1 < i.stack.indexOf(`
    at`) ? " (<anonymous>)" : -1 < i.stack.indexOf("@") ? "@unknown:0:0" : "";
            }
            return `
` + io + t + Ed;
        }
        var lo = !1;
        function so(t, e) {
            if (!t || lo) return "";
            lo = !0;
            var i = Error.prepareStackTrace;
            Error.prepareStackTrace = void 0;
            try {
                var s = {
                    DetermineComponentFrameRoot: function() {
                        try {
                            if (e) {
                                var K = function() {
                                    throw Error();
                                };
                                if (Object.defineProperty(K.prototype, "props", {
                                    set: function() {
                                        throw Error();
                                    }
                                }), typeof Reflect == "object" && Reflect.construct) {
                                    try {
                                        Reflect.construct(K, []);
                                    } catch (L) {
                                        var N = L;
                                    }
                                    Reflect.construct(t, [], K);
                                } else {
                                    try {
                                        K.call();
                                    } catch (L) {
                                        N = L;
                                    }
                                    t.call(K.prototype);
                                }
                            } else {
                                try {
                                    throw Error();
                                } catch (L) {
                                    N = L;
                                }
                                (K = t()) && typeof K.catch == "function" && K.catch(function() {});
                            }
                        } catch (L) {
                            if (L && N && typeof L.stack == "string") return [
                                L.stack,
                                N.stack
                            ];
                        }
                        return [
                            null,
                            null
                        ];
                    }
                };
                s.DetermineComponentFrameRoot.displayName = "DetermineComponentFrameRoot";
                var o = Object.getOwnPropertyDescriptor(s.DetermineComponentFrameRoot, "name");
                o && o.configurable && Object.defineProperty(s.DetermineComponentFrameRoot, "name", {
                    value: "DetermineComponentFrameRoot"
                });
                var c = s.DetermineComponentFrameRoot(), g = c[0], b = c[1];
                if (g && b) {
                    var C = g.split(`
`), O = b.split(`
`);
                    for(o = s = 0; s < C.length && !C[s].includes("DetermineComponentFrameRoot");)s++;
                    for(; o < O.length && !O[o].includes("DetermineComponentFrameRoot");)o++;
                    if (s === C.length || o === O.length) for(s = C.length - 1, o = O.length - 1; 1 <= s && 0 <= o && C[s] !== O[o];)o--;
                    for(; 1 <= s && 0 <= o; s--, o--)if (C[s] !== O[o]) {
                        if (s !== 1 || o !== 1) do if (s--, o--, 0 > o || C[s] !== O[o]) {
                            var q = `
` + C[s].replace(" at new ", " at ");
                            return t.displayName && q.includes("<anonymous>") && (q = q.replace("<anonymous>", t.displayName)), q;
                        }
                        while (1 <= s && 0 <= o);
                        break;
                    }
                }
            } finally{
                lo = !1, Error.prepareStackTrace = i;
            }
            return (i = t ? t.displayName || t.name : "") ? fa(i) : "";
        }
        function kb(t, e) {
            switch(t.tag){
                case 26:
                case 27:
                case 5:
                    return fa(t.type);
                case 16:
                    return fa("Lazy");
                case 13:
                    return t.child !== e && e !== null ? fa("Suspense Fallback") : fa("Suspense");
                case 19:
                    return fa("SuspenseList");
                case 0:
                case 15:
                    return so(t.type, !1);
                case 11:
                    return so(t.type.render, !1);
                case 1:
                    return so(t.type, !0);
                case 31:
                    return fa("Activity");
                default:
                    return "";
            }
        }
        function Ad(t) {
            try {
                var e = "", i = null;
                do e += kb(t, i), i = t, t = t.return;
                while (t);
                return e;
            } catch (s) {
                return `
Error generating stack: ` + s.message + `
` + s.stack;
            }
        }
        var ro = Object.prototype.hasOwnProperty, oo = n.unstable_scheduleCallback, uo = n.unstable_cancelCallback, Yb = n.unstable_shouldYield, Xb = n.unstable_requestPaint, Ae = n.unstable_now, Kb = n.unstable_getCurrentPriorityLevel, Cd = n.unstable_ImmediatePriority, wd = n.unstable_UserBlockingPriority, is = n.unstable_NormalPriority, Pb = n.unstable_LowPriority, _d = n.unstable_IdlePriority, Zb = n.log, Qb = n.unstable_setDisableYieldValue, Vi = null, Ce = null;
        function zn(t) {
            if (typeof Zb == "function" && Qb(t), Ce && typeof Ce.setStrictMode == "function") try {
                Ce.setStrictMode(Vi, t);
            } catch  {}
        }
        var we = Math.clz32 ? Math.clz32 : Jb, Fb = Math.log, $b = Math.LN2;
        function Jb(t) {
            return t >>>= 0, t === 0 ? 32 : 31 - (Fb(t) / $b | 0) | 0;
        }
        var ls = 256, ss = 262144, rs = 4194304;
        function da(t) {
            var e = t & 42;
            if (e !== 0) return e;
            switch(t & -t){
                case 1:
                    return 1;
                case 2:
                    return 2;
                case 4:
                    return 4;
                case 8:
                    return 8;
                case 16:
                    return 16;
                case 32:
                    return 32;
                case 64:
                    return 64;
                case 128:
                    return 128;
                case 256:
                case 512:
                case 1024:
                case 2048:
                case 4096:
                case 8192:
                case 16384:
                case 32768:
                case 65536:
                case 131072:
                    return t & 261888;
                case 262144:
                case 524288:
                case 1048576:
                case 2097152:
                    return t & 3932160;
                case 4194304:
                case 8388608:
                case 16777216:
                case 33554432:
                    return t & 62914560;
                case 67108864:
                    return 67108864;
                case 134217728:
                    return 134217728;
                case 268435456:
                    return 268435456;
                case 536870912:
                    return 536870912;
                case 1073741824:
                    return 0;
                default:
                    return t;
            }
        }
        function os(t, e, i) {
            var s = t.pendingLanes;
            if (s === 0) return 0;
            var o = 0, c = t.suspendedLanes, g = t.pingedLanes;
            t = t.warmLanes;
            var b = s & 134217727;
            return b !== 0 ? (s = b & ~c, s !== 0 ? o = da(s) : (g &= b, g !== 0 ? o = da(g) : i || (i = b & ~t, i !== 0 && (o = da(i))))) : (b = s & ~c, b !== 0 ? o = da(b) : g !== 0 ? o = da(g) : i || (i = s & ~t, i !== 0 && (o = da(i)))), o === 0 ? 0 : e !== 0 && e !== o && (e & c) === 0 && (c = o & -o, i = e & -e, c >= i || c === 32 && (i & 4194048) !== 0) ? e : o;
        }
        function Bi(t, e) {
            return (t.pendingLanes & ~(t.suspendedLanes & ~t.pingedLanes) & e) === 0;
        }
        function Wb(t, e) {
            switch(t){
                case 1:
                case 2:
                case 4:
                case 8:
                case 64:
                    return e + 250;
                case 16:
                case 32:
                case 128:
                case 256:
                case 512:
                case 1024:
                case 2048:
                case 4096:
                case 8192:
                case 16384:
                case 32768:
                case 65536:
                case 131072:
                case 262144:
                case 524288:
                case 1048576:
                case 2097152:
                    return e + 5e3;
                case 4194304:
                case 8388608:
                case 16777216:
                case 33554432:
                    return -1;
                case 67108864:
                case 134217728:
                case 268435456:
                case 536870912:
                case 1073741824:
                    return -1;
                default:
                    return -1;
            }
        }
        function Rd() {
            var t = rs;
            return rs <<= 1, (rs & 62914560) === 0 && (rs = 4194304), t;
        }
        function co(t) {
            for(var e = [], i = 0; 31 > i; i++)e.push(t);
            return e;
        }
        function Ui(t, e) {
            t.pendingLanes |= e, e !== 268435456 && (t.suspendedLanes = 0, t.pingedLanes = 0, t.warmLanes = 0);
        }
        function Ib(t, e, i, s, o, c) {
            var g = t.pendingLanes;
            t.pendingLanes = i, t.suspendedLanes = 0, t.pingedLanes = 0, t.warmLanes = 0, t.expiredLanes &= i, t.entangledLanes &= i, t.errorRecoveryDisabledLanes &= i, t.shellSuspendCounter = 0;
            var b = t.entanglements, C = t.expirationTimes, O = t.hiddenUpdates;
            for(i = g & ~i; 0 < i;){
                var q = 31 - we(i), K = 1 << q;
                b[q] = 0, C[q] = -1;
                var N = O[q];
                if (N !== null) for(O[q] = null, q = 0; q < N.length; q++){
                    var L = N[q];
                    L !== null && (L.lane &= -536870913);
                }
                i &= ~K;
            }
            s !== 0 && Md(t, s, 0), c !== 0 && o === 0 && t.tag !== 0 && (t.suspendedLanes |= c & ~(g & ~e));
        }
        function Md(t, e, i) {
            t.pendingLanes |= e, t.suspendedLanes &= ~e;
            var s = 31 - we(e);
            t.entangledLanes |= e, t.entanglements[s] = t.entanglements[s] | 1073741824 | i & 261930;
        }
        function Dd(t, e) {
            var i = t.entangledLanes |= e;
            for(t = t.entanglements; i;){
                var s = 31 - we(i), o = 1 << s;
                o & e | t[s] & e && (t[s] |= e), i &= ~o;
            }
        }
        function jd(t, e) {
            var i = e & -e;
            return i = (i & 42) !== 0 ? 1 : fo(i), (i & (t.suspendedLanes | e)) !== 0 ? 0 : i;
        }
        function fo(t) {
            switch(t){
                case 2:
                    t = 1;
                    break;
                case 8:
                    t = 4;
                    break;
                case 32:
                    t = 16;
                    break;
                case 256:
                case 512:
                case 1024:
                case 2048:
                case 4096:
                case 8192:
                case 16384:
                case 32768:
                case 65536:
                case 131072:
                case 262144:
                case 524288:
                case 1048576:
                case 2097152:
                case 4194304:
                case 8388608:
                case 16777216:
                case 33554432:
                    t = 128;
                    break;
                case 268435456:
                    t = 134217728;
                    break;
                default:
                    t = 0;
            }
            return t;
        }
        function ho(t) {
            return t &= -t, 2 < t ? 8 < t ? (t & 134217727) !== 0 ? 32 : 268435456 : 8 : 2;
        }
        function Od() {
            var t = F.p;
            return t !== 0 ? t : (t = window.event, t === void 0 ? 32 : yg(t.type));
        }
        function Nd(t, e) {
            var i = F.p;
            try {
                return F.p = t, e();
            } finally{
                F.p = i;
            }
        }
        var Ln = Math.random().toString(36).slice(2), le = "__reactFiber$" + Ln, ye = "__reactProps$" + Ln, Va = "__reactContainer$" + Ln, mo = "__reactEvents$" + Ln, tx = "__reactListeners$" + Ln, ex = "__reactHandles$" + Ln, zd = "__reactResources$" + Ln, Hi = "__reactMarker$" + Ln;
        function po(t) {
            delete t[le], delete t[ye], delete t[mo], delete t[tx], delete t[ex];
        }
        function Ba(t) {
            var e = t[le];
            if (e) return e;
            for(var i = t.parentNode; i;){
                if (e = i[Va] || i[le]) {
                    if (i = e.alternate, e.child !== null || i !== null && i.child !== null) for(t = eg(t); t !== null;){
                        if (i = t[le]) return i;
                        t = eg(t);
                    }
                    return e;
                }
                t = i, i = t.parentNode;
            }
            return null;
        }
        function Ua(t) {
            if (t = t[le] || t[Va]) {
                var e = t.tag;
                if (e === 5 || e === 6 || e === 13 || e === 31 || e === 26 || e === 27 || e === 3) return t;
            }
            return null;
        }
        function Gi(t) {
            var e = t.tag;
            if (e === 5 || e === 26 || e === 27 || e === 6) return t.stateNode;
            throw Error(r(33));
        }
        function Ha(t) {
            var e = t[zd];
            return e || (e = t[zd] = {
                hoistableStyles: new Map,
                hoistableScripts: new Map
            }), e;
        }
        function ne(t) {
            t[Hi] = !0;
        }
        var Ld = new Set, Vd = {};
        function ha(t, e) {
            Ga(t, e), Ga(t + "Capture", e);
        }
        function Ga(t, e) {
            for(Vd[t] = e, t = 0; t < e.length; t++)Ld.add(e[t]);
        }
        var nx = RegExp("^[:A-Z_a-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD][:A-Z_a-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD\\-.0-9\\u00B7\\u0300-\\u036F\\u203F-\\u2040]*$"), Bd = {}, Ud = {};
        function ax(t) {
            return ro.call(Ud, t) ? !0 : ro.call(Bd, t) ? !1 : nx.test(t) ? Ud[t] = !0 : (Bd[t] = !0, !1);
        }
        function us(t, e, i) {
            if (ax(e)) if (i === null) t.removeAttribute(e);
            else {
                switch(typeof i){
                    case "undefined":
                    case "function":
                    case "symbol":
                        t.removeAttribute(e);
                        return;
                    case "boolean":
                        var s = e.toLowerCase().slice(0, 5);
                        if (s !== "data-" && s !== "aria-") {
                            t.removeAttribute(e);
                            return;
                        }
                }
                t.setAttribute(e, "" + i);
            }
        }
        function cs(t, e, i) {
            if (i === null) t.removeAttribute(e);
            else {
                switch(typeof i){
                    case "undefined":
                    case "function":
                    case "symbol":
                    case "boolean":
                        t.removeAttribute(e);
                        return;
                }
                t.setAttribute(e, "" + i);
            }
        }
        function cn(t, e, i, s) {
            if (s === null) t.removeAttribute(i);
            else {
                switch(typeof s){
                    case "undefined":
                    case "function":
                    case "symbol":
                    case "boolean":
                        t.removeAttribute(i);
                        return;
                }
                t.setAttributeNS(e, i, "" + s);
            }
        }
        function ze(t) {
            switch(typeof t){
                case "bigint":
                case "boolean":
                case "number":
                case "string":
                case "undefined":
                    return t;
                case "object":
                    return t;
                default:
                    return "";
            }
        }
        function Hd(t) {
            var e = t.type;
            return (t = t.nodeName) && t.toLowerCase() === "input" && (e === "checkbox" || e === "radio");
        }
        function ix(t, e, i) {
            var s = Object.getOwnPropertyDescriptor(t.constructor.prototype, e);
            if (!t.hasOwnProperty(e) && typeof s < "u" && typeof s.get == "function" && typeof s.set == "function") {
                var o = s.get, c = s.set;
                return Object.defineProperty(t, e, {
                    configurable: !0,
                    get: function() {
                        return o.call(this);
                    },
                    set: function(g) {
                        i = "" + g, c.call(this, g);
                    }
                }), Object.defineProperty(t, e, {
                    enumerable: s.enumerable
                }), {
                    getValue: function() {
                        return i;
                    },
                    setValue: function(g) {
                        i = "" + g;
                    },
                    stopTracking: function() {
                        t._valueTracker = null, delete t[e];
                    }
                };
            }
        }
        function go(t) {
            if (!t._valueTracker) {
                var e = Hd(t) ? "checked" : "value";
                t._valueTracker = ix(t, e, "" + t[e]);
            }
        }
        function Gd(t) {
            if (!t) return !1;
            var e = t._valueTracker;
            if (!e) return !0;
            var i = e.getValue(), s = "";
            return t && (s = Hd(t) ? t.checked ? "true" : "false" : t.value), t = s, t !== i ? (e.setValue(t), !0) : !1;
        }
        function fs(t) {
            if (t = t || (typeof document < "u" ? document : void 0), typeof t > "u") return null;
            try {
                return t.activeElement || t.body;
            } catch  {
                return t.body;
            }
        }
        var lx = /[\n"\\]/g;
        function Le(t) {
            return t.replace(lx, function(e) {
                return "\\" + e.charCodeAt(0).toString(16) + " ";
            });
        }
        function yo(t, e, i, s, o, c, g, b) {
            t.name = "", g != null && typeof g != "function" && typeof g != "symbol" && typeof g != "boolean" ? t.type = g : t.removeAttribute("type"), e != null ? g === "number" ? (e === 0 && t.value === "" || t.value != e) && (t.value = "" + ze(e)) : t.value !== "" + ze(e) && (t.value = "" + ze(e)) : g !== "submit" && g !== "reset" || t.removeAttribute("value"), e != null ? vo(t, g, ze(e)) : i != null ? vo(t, g, ze(i)) : s != null && t.removeAttribute("value"), o == null && c != null && (t.defaultChecked = !!c), o != null && (t.checked = o && typeof o != "function" && typeof o != "symbol"), b != null && typeof b != "function" && typeof b != "symbol" && typeof b != "boolean" ? t.name = "" + ze(b) : t.removeAttribute("name");
        }
        function qd(t, e, i, s, o, c, g, b) {
            if (c != null && typeof c != "function" && typeof c != "symbol" && typeof c != "boolean" && (t.type = c), e != null || i != null) {
                if (!(c !== "submit" && c !== "reset" || e != null)) {
                    go(t);
                    return;
                }
                i = i != null ? "" + ze(i) : "", e = e != null ? "" + ze(e) : i, b || e === t.value || (t.value = e), t.defaultValue = e;
            }
            s = s ?? o, s = typeof s != "function" && typeof s != "symbol" && !!s, t.checked = b ? t.checked : !!s, t.defaultChecked = !!s, g != null && typeof g != "function" && typeof g != "symbol" && typeof g != "boolean" && (t.name = g), go(t);
        }
        function vo(t, e, i) {
            e === "number" && fs(t.ownerDocument) === t || t.defaultValue === "" + i || (t.defaultValue = "" + i);
        }
        function qa(t, e, i, s) {
            if (t = t.options, e) {
                e = {};
                for(var o = 0; o < i.length; o++)e["$" + i[o]] = !0;
                for(i = 0; i < t.length; i++)o = e.hasOwnProperty("$" + t[i].value), t[i].selected !== o && (t[i].selected = o), o && s && (t[i].defaultSelected = !0);
            } else {
                for(i = "" + ze(i), e = null, o = 0; o < t.length; o++){
                    if (t[o].value === i) {
                        t[o].selected = !0, s && (t[o].defaultSelected = !0);
                        return;
                    }
                    e !== null || t[o].disabled || (e = t[o]);
                }
                e !== null && (e.selected = !0);
            }
        }
        function kd(t, e, i) {
            if (e != null && (e = "" + ze(e), e !== t.value && (t.value = e), i == null)) {
                t.defaultValue !== e && (t.defaultValue = e);
                return;
            }
            t.defaultValue = i != null ? "" + ze(i) : "";
        }
        function Yd(t, e, i, s) {
            if (e == null) {
                if (s != null) {
                    if (i != null) throw Error(r(92));
                    if (Vt(s)) {
                        if (1 < s.length) throw Error(r(93));
                        s = s[0];
                    }
                    i = s;
                }
                i == null && (i = ""), e = i;
            }
            i = ze(e), t.defaultValue = i, s = t.textContent, s === i && s !== "" && s !== null && (t.value = s), go(t);
        }
        function ka(t, e) {
            if (e) {
                var i = t.firstChild;
                if (i && i === t.lastChild && i.nodeType === 3) {
                    i.nodeValue = e;
                    return;
                }
            }
            t.textContent = e;
        }
        var sx = new Set("animationIterationCount aspectRatio borderImageOutset borderImageSlice borderImageWidth boxFlex boxFlexGroup boxOrdinalGroup columnCount columns flex flexGrow flexPositive flexShrink flexNegative flexOrder gridArea gridRow gridRowEnd gridRowSpan gridRowStart gridColumn gridColumnEnd gridColumnSpan gridColumnStart fontWeight lineClamp lineHeight opacity order orphans scale tabSize widows zIndex zoom fillOpacity floodOpacity stopOpacity strokeDasharray strokeDashoffset strokeMiterlimit strokeOpacity strokeWidth MozAnimationIterationCount MozBoxFlex MozBoxFlexGroup MozLineClamp msAnimationIterationCount msFlex msZoom msFlexGrow msFlexNegative msFlexOrder msFlexPositive msFlexShrink msGridColumn msGridColumnSpan msGridRow msGridRowSpan WebkitAnimationIterationCount WebkitBoxFlex WebKitBoxFlexGroup WebkitBoxOrdinalGroup WebkitColumnCount WebkitColumns WebkitFlex WebkitFlexGrow WebkitFlexPositive WebkitFlexShrink WebkitLineClamp".split(" "));
        function Xd(t, e, i) {
            var s = e.indexOf("--") === 0;
            i == null || typeof i == "boolean" || i === "" ? s ? t.setProperty(e, "") : e === "float" ? t.cssFloat = "" : t[e] = "" : s ? t.setProperty(e, i) : typeof i != "number" || i === 0 || sx.has(e) ? e === "float" ? t.cssFloat = i : t[e] = ("" + i).trim() : t[e] = i + "px";
        }
        function Kd(t, e, i) {
            if (e != null && typeof e != "object") throw Error(r(62));
            if (t = t.style, i != null) {
                for(var s in i)!i.hasOwnProperty(s) || e != null && e.hasOwnProperty(s) || (s.indexOf("--") === 0 ? t.setProperty(s, "") : s === "float" ? t.cssFloat = "" : t[s] = "");
                for(var o in e)s = e[o], e.hasOwnProperty(o) && i[o] !== s && Xd(t, o, s);
            } else for(var c in e)e.hasOwnProperty(c) && Xd(t, c, e[c]);
        }
        function bo(t) {
            if (t.indexOf("-") === -1) return !1;
            switch(t){
                case "annotation-xml":
                case "color-profile":
                case "font-face":
                case "font-face-src":
                case "font-face-uri":
                case "font-face-format":
                case "font-face-name":
                case "missing-glyph":
                    return !1;
                default:
                    return !0;
            }
        }
        var rx = new Map([
            [
                "acceptCharset",
                "accept-charset"
            ],
            [
                "htmlFor",
                "for"
            ],
            [
                "httpEquiv",
                "http-equiv"
            ],
            [
                "crossOrigin",
                "crossorigin"
            ],
            [
                "accentHeight",
                "accent-height"
            ],
            [
                "alignmentBaseline",
                "alignment-baseline"
            ],
            [
                "arabicForm",
                "arabic-form"
            ],
            [
                "baselineShift",
                "baseline-shift"
            ],
            [
                "capHeight",
                "cap-height"
            ],
            [
                "clipPath",
                "clip-path"
            ],
            [
                "clipRule",
                "clip-rule"
            ],
            [
                "colorInterpolation",
                "color-interpolation"
            ],
            [
                "colorInterpolationFilters",
                "color-interpolation-filters"
            ],
            [
                "colorProfile",
                "color-profile"
            ],
            [
                "colorRendering",
                "color-rendering"
            ],
            [
                "dominantBaseline",
                "dominant-baseline"
            ],
            [
                "enableBackground",
                "enable-background"
            ],
            [
                "fillOpacity",
                "fill-opacity"
            ],
            [
                "fillRule",
                "fill-rule"
            ],
            [
                "floodColor",
                "flood-color"
            ],
            [
                "floodOpacity",
                "flood-opacity"
            ],
            [
                "fontFamily",
                "font-family"
            ],
            [
                "fontSize",
                "font-size"
            ],
            [
                "fontSizeAdjust",
                "font-size-adjust"
            ],
            [
                "fontStretch",
                "font-stretch"
            ],
            [
                "fontStyle",
                "font-style"
            ],
            [
                "fontVariant",
                "font-variant"
            ],
            [
                "fontWeight",
                "font-weight"
            ],
            [
                "glyphName",
                "glyph-name"
            ],
            [
                "glyphOrientationHorizontal",
                "glyph-orientation-horizontal"
            ],
            [
                "glyphOrientationVertical",
                "glyph-orientation-vertical"
            ],
            [
                "horizAdvX",
                "horiz-adv-x"
            ],
            [
                "horizOriginX",
                "horiz-origin-x"
            ],
            [
                "imageRendering",
                "image-rendering"
            ],
            [
                "letterSpacing",
                "letter-spacing"
            ],
            [
                "lightingColor",
                "lighting-color"
            ],
            [
                "markerEnd",
                "marker-end"
            ],
            [
                "markerMid",
                "marker-mid"
            ],
            [
                "markerStart",
                "marker-start"
            ],
            [
                "overlinePosition",
                "overline-position"
            ],
            [
                "overlineThickness",
                "overline-thickness"
            ],
            [
                "paintOrder",
                "paint-order"
            ],
            [
                "panose-1",
                "panose-1"
            ],
            [
                "pointerEvents",
                "pointer-events"
            ],
            [
                "renderingIntent",
                "rendering-intent"
            ],
            [
                "shapeRendering",
                "shape-rendering"
            ],
            [
                "stopColor",
                "stop-color"
            ],
            [
                "stopOpacity",
                "stop-opacity"
            ],
            [
                "strikethroughPosition",
                "strikethrough-position"
            ],
            [
                "strikethroughThickness",
                "strikethrough-thickness"
            ],
            [
                "strokeDasharray",
                "stroke-dasharray"
            ],
            [
                "strokeDashoffset",
                "stroke-dashoffset"
            ],
            [
                "strokeLinecap",
                "stroke-linecap"
            ],
            [
                "strokeLinejoin",
                "stroke-linejoin"
            ],
            [
                "strokeMiterlimit",
                "stroke-miterlimit"
            ],
            [
                "strokeOpacity",
                "stroke-opacity"
            ],
            [
                "strokeWidth",
                "stroke-width"
            ],
            [
                "textAnchor",
                "text-anchor"
            ],
            [
                "textDecoration",
                "text-decoration"
            ],
            [
                "textRendering",
                "text-rendering"
            ],
            [
                "transformOrigin",
                "transform-origin"
            ],
            [
                "underlinePosition",
                "underline-position"
            ],
            [
                "underlineThickness",
                "underline-thickness"
            ],
            [
                "unicodeBidi",
                "unicode-bidi"
            ],
            [
                "unicodeRange",
                "unicode-range"
            ],
            [
                "unitsPerEm",
                "units-per-em"
            ],
            [
                "vAlphabetic",
                "v-alphabetic"
            ],
            [
                "vHanging",
                "v-hanging"
            ],
            [
                "vIdeographic",
                "v-ideographic"
            ],
            [
                "vMathematical",
                "v-mathematical"
            ],
            [
                "vectorEffect",
                "vector-effect"
            ],
            [
                "vertAdvY",
                "vert-adv-y"
            ],
            [
                "vertOriginX",
                "vert-origin-x"
            ],
            [
                "vertOriginY",
                "vert-origin-y"
            ],
            [
                "wordSpacing",
                "word-spacing"
            ],
            [
                "writingMode",
                "writing-mode"
            ],
            [
                "xmlnsXlink",
                "xmlns:xlink"
            ],
            [
                "xHeight",
                "x-height"
            ]
        ]), ox = /^[\u0000-\u001F ]*j[\r\n\t]*a[\r\n\t]*v[\r\n\t]*a[\r\n\t]*s[\r\n\t]*c[\r\n\t]*r[\r\n\t]*i[\r\n\t]*p[\r\n\t]*t[\r\n\t]*:/i;
        function ds(t) {
            return ox.test("" + t) ? "javascript:throw new Error('React has blocked a javascript: URL as a security precaution.')" : t;
        }
        function fn() {}
        var xo = null;
        function So(t) {
            return t = t.target || t.srcElement || window, t.correspondingUseElement && (t = t.correspondingUseElement), t.nodeType === 3 ? t.parentNode : t;
        }
        var Ya = null, Xa = null;
        function Pd(t) {
            var e = Ua(t);
            if (e && (t = e.stateNode)) {
                var i = t[ye] || null;
                t: switch(t = e.stateNode, e.type){
                    case "input":
                        if (yo(t, i.value, i.defaultValue, i.defaultValue, i.checked, i.defaultChecked, i.type, i.name), e = i.name, i.type === "radio" && e != null) {
                            for(i = t; i.parentNode;)i = i.parentNode;
                            for(i = i.querySelectorAll('input[name="' + Le("" + e) + '"][type="radio"]'), e = 0; e < i.length; e++){
                                var s = i[e];
                                if (s !== t && s.form === t.form) {
                                    var o = s[ye] || null;
                                    if (!o) throw Error(r(90));
                                    yo(s, o.value, o.defaultValue, o.defaultValue, o.checked, o.defaultChecked, o.type, o.name);
                                }
                            }
                            for(e = 0; e < i.length; e++)s = i[e], s.form === t.form && Gd(s);
                        }
                        break t;
                    case "textarea":
                        kd(t, i.value, i.defaultValue);
                        break t;
                    case "select":
                        e = i.value, e != null && qa(t, !!i.multiple, e, !1);
                }
            }
        }
        var To = !1;
        function Zd(t, e, i) {
            if (To) return t(e, i);
            To = !0;
            try {
                var s = t(e);
                return s;
            } finally{
                if (To = !1, (Ya !== null || Xa !== null) && (Is(), Ya && (e = Ya, t = Xa, Xa = Ya = null, Pd(e), t))) for(e = 0; e < t.length; e++)Pd(t[e]);
            }
        }
        function qi(t, e) {
            var i = t.stateNode;
            if (i === null) return null;
            var s = i[ye] || null;
            if (s === null) return null;
            i = s[e];
            t: switch(e){
                case "onClick":
                case "onClickCapture":
                case "onDoubleClick":
                case "onDoubleClickCapture":
                case "onMouseDown":
                case "onMouseDownCapture":
                case "onMouseMove":
                case "onMouseMoveCapture":
                case "onMouseUp":
                case "onMouseUpCapture":
                case "onMouseEnter":
                    (s = !s.disabled) || (t = t.type, s = !(t === "button" || t === "input" || t === "select" || t === "textarea")), t = !s;
                    break t;
                default:
                    t = !1;
            }
            if (t) return null;
            if (i && typeof i != "function") throw Error(r(231, e, typeof i));
            return i;
        }
        var dn = !(typeof window > "u" || typeof window.document > "u" || typeof window.document.createElement > "u"), Eo = !1;
        if (dn) try {
            var ki = {};
            Object.defineProperty(ki, "passive", {
                get: function() {
                    Eo = !0;
                }
            }), window.addEventListener("test", ki, ki), window.removeEventListener("test", ki, ki);
        } catch  {
            Eo = !1;
        }
        var Vn = null, Ao = null, hs = null;
        function Qd() {
            if (hs) return hs;
            var t, e = Ao, i = e.length, s, o = "value" in Vn ? Vn.value : Vn.textContent, c = o.length;
            for(t = 0; t < i && e[t] === o[t]; t++);
            var g = i - t;
            for(s = 1; s <= g && e[i - s] === o[c - s]; s++);
            return hs = o.slice(t, 1 < s ? 1 - s : void 0);
        }
        function ms(t) {
            var e = t.keyCode;
            return "charCode" in t ? (t = t.charCode, t === 0 && e === 13 && (t = 13)) : t = e, t === 10 && (t = 13), 32 <= t || t === 13 ? t : 0;
        }
        function ps() {
            return !0;
        }
        function Fd() {
            return !1;
        }
        function ve(t) {
            function e(i, s, o, c, g) {
                this._reactName = i, this._targetInst = o, this.type = s, this.nativeEvent = c, this.target = g, this.currentTarget = null;
                for(var b in t)t.hasOwnProperty(b) && (i = t[b], this[b] = i ? i(c) : c[b]);
                return this.isDefaultPrevented = (c.defaultPrevented != null ? c.defaultPrevented : c.returnValue === !1) ? ps : Fd, this.isPropagationStopped = Fd, this;
            }
            return v(e.prototype, {
                preventDefault: function() {
                    this.defaultPrevented = !0;
                    var i = this.nativeEvent;
                    i && (i.preventDefault ? i.preventDefault() : typeof i.returnValue != "unknown" && (i.returnValue = !1), this.isDefaultPrevented = ps);
                },
                stopPropagation: function() {
                    var i = this.nativeEvent;
                    i && (i.stopPropagation ? i.stopPropagation() : typeof i.cancelBubble != "unknown" && (i.cancelBubble = !0), this.isPropagationStopped = ps);
                },
                persist: function() {},
                isPersistent: ps
            }), e;
        }
        var ma = {
            eventPhase: 0,
            bubbles: 0,
            cancelable: 0,
            timeStamp: function(t) {
                return t.timeStamp || Date.now();
            },
            defaultPrevented: 0,
            isTrusted: 0
        }, gs = ve(ma), Yi = v({}, ma, {
            view: 0,
            detail: 0
        }), ux = ve(Yi), Co, wo, Xi, ys = v({}, Yi, {
            screenX: 0,
            screenY: 0,
            clientX: 0,
            clientY: 0,
            pageX: 0,
            pageY: 0,
            ctrlKey: 0,
            shiftKey: 0,
            altKey: 0,
            metaKey: 0,
            getModifierState: Ro,
            button: 0,
            buttons: 0,
            relatedTarget: function(t) {
                return t.relatedTarget === void 0 ? t.fromElement === t.srcElement ? t.toElement : t.fromElement : t.relatedTarget;
            },
            movementX: function(t) {
                return "movementX" in t ? t.movementX : (t !== Xi && (Xi && t.type === "mousemove" ? (Co = t.screenX - Xi.screenX, wo = t.screenY - Xi.screenY) : wo = Co = 0, Xi = t), Co);
            },
            movementY: function(t) {
                return "movementY" in t ? t.movementY : wo;
            }
        }), $d = ve(ys), cx = v({}, ys, {
            dataTransfer: 0
        }), fx = ve(cx), dx = v({}, Yi, {
            relatedTarget: 0
        }), _o = ve(dx), hx = v({}, ma, {
            animationName: 0,
            elapsedTime: 0,
            pseudoElement: 0
        }), mx = ve(hx), px = v({}, ma, {
            clipboardData: function(t) {
                return "clipboardData" in t ? t.clipboardData : window.clipboardData;
            }
        }), gx = ve(px), yx = v({}, ma, {
            data: 0
        }), Jd = ve(yx), vx = {
            Esc: "Escape",
            Spacebar: " ",
            Left: "ArrowLeft",
            Up: "ArrowUp",
            Right: "ArrowRight",
            Down: "ArrowDown",
            Del: "Delete",
            Win: "OS",
            Menu: "ContextMenu",
            Apps: "ContextMenu",
            Scroll: "ScrollLock",
            MozPrintableKey: "Unidentified"
        }, bx = {
            8: "Backspace",
            9: "Tab",
            12: "Clear",
            13: "Enter",
            16: "Shift",
            17: "Control",
            18: "Alt",
            19: "Pause",
            20: "CapsLock",
            27: "Escape",
            32: " ",
            33: "PageUp",
            34: "PageDown",
            35: "End",
            36: "Home",
            37: "ArrowLeft",
            38: "ArrowUp",
            39: "ArrowRight",
            40: "ArrowDown",
            45: "Insert",
            46: "Delete",
            112: "F1",
            113: "F2",
            114: "F3",
            115: "F4",
            116: "F5",
            117: "F6",
            118: "F7",
            119: "F8",
            120: "F9",
            121: "F10",
            122: "F11",
            123: "F12",
            144: "NumLock",
            145: "ScrollLock",
            224: "Meta"
        }, xx = {
            Alt: "altKey",
            Control: "ctrlKey",
            Meta: "metaKey",
            Shift: "shiftKey"
        };
        function Sx(t) {
            var e = this.nativeEvent;
            return e.getModifierState ? e.getModifierState(t) : (t = xx[t]) ? !!e[t] : !1;
        }
        function Ro() {
            return Sx;
        }
        var Tx = v({}, Yi, {
            key: function(t) {
                if (t.key) {
                    var e = vx[t.key] || t.key;
                    if (e !== "Unidentified") return e;
                }
                return t.type === "keypress" ? (t = ms(t), t === 13 ? "Enter" : String.fromCharCode(t)) : t.type === "keydown" || t.type === "keyup" ? bx[t.keyCode] || "Unidentified" : "";
            },
            code: 0,
            location: 0,
            ctrlKey: 0,
            shiftKey: 0,
            altKey: 0,
            metaKey: 0,
            repeat: 0,
            locale: 0,
            getModifierState: Ro,
            charCode: function(t) {
                return t.type === "keypress" ? ms(t) : 0;
            },
            keyCode: function(t) {
                return t.type === "keydown" || t.type === "keyup" ? t.keyCode : 0;
            },
            which: function(t) {
                return t.type === "keypress" ? ms(t) : t.type === "keydown" || t.type === "keyup" ? t.keyCode : 0;
            }
        }), Ex = ve(Tx), Ax = v({}, ys, {
            pointerId: 0,
            width: 0,
            height: 0,
            pressure: 0,
            tangentialPressure: 0,
            tiltX: 0,
            tiltY: 0,
            twist: 0,
            pointerType: 0,
            isPrimary: 0
        }), Wd = ve(Ax), Cx = v({}, Yi, {
            touches: 0,
            targetTouches: 0,
            changedTouches: 0,
            altKey: 0,
            metaKey: 0,
            ctrlKey: 0,
            shiftKey: 0,
            getModifierState: Ro
        }), wx = ve(Cx), _x = v({}, ma, {
            propertyName: 0,
            elapsedTime: 0,
            pseudoElement: 0
        }), Rx = ve(_x), Mx = v({}, ys, {
            deltaX: function(t) {
                return "deltaX" in t ? t.deltaX : "wheelDeltaX" in t ? -t.wheelDeltaX : 0;
            },
            deltaY: function(t) {
                return "deltaY" in t ? t.deltaY : "wheelDeltaY" in t ? -t.wheelDeltaY : "wheelDelta" in t ? -t.wheelDelta : 0;
            },
            deltaZ: 0,
            deltaMode: 0
        }), Dx = ve(Mx), jx = v({}, ma, {
            newState: 0,
            oldState: 0
        }), Ox = ve(jx), Nx = [
            9,
            13,
            27,
            32
        ], Mo = dn && "CompositionEvent" in window, Ki = null;
        dn && "documentMode" in document && (Ki = document.documentMode);
        var zx = dn && "TextEvent" in window && !Ki, Id = dn && (!Mo || Ki && 8 < Ki && 11 >= Ki), th = " ", eh = !1;
        function nh(t, e) {
            switch(t){
                case "keyup":
                    return Nx.indexOf(e.keyCode) !== -1;
                case "keydown":
                    return e.keyCode !== 229;
                case "keypress":
                case "mousedown":
                case "focusout":
                    return !0;
                default:
                    return !1;
            }
        }
        function ah(t) {
            return t = t.detail, typeof t == "object" && "data" in t ? t.data : null;
        }
        var Ka = !1;
        function Lx(t, e) {
            switch(t){
                case "compositionend":
                    return ah(e);
                case "keypress":
                    return e.which !== 32 ? null : (eh = !0, th);
                case "textInput":
                    return t = e.data, t === th && eh ? null : t;
                default:
                    return null;
            }
        }
        function Vx(t, e) {
            if (Ka) return t === "compositionend" || !Mo && nh(t, e) ? (t = Qd(), hs = Ao = Vn = null, Ka = !1, t) : null;
            switch(t){
                case "paste":
                    return null;
                case "keypress":
                    if (!(e.ctrlKey || e.altKey || e.metaKey) || e.ctrlKey && e.altKey) {
                        if (e.char && 1 < e.char.length) return e.char;
                        if (e.which) return String.fromCharCode(e.which);
                    }
                    return null;
                case "compositionend":
                    return Id && e.locale !== "ko" ? null : e.data;
                default:
                    return null;
            }
        }
        var Bx = {
            color: !0,
            date: !0,
            datetime: !0,
            "datetime-local": !0,
            email: !0,
            month: !0,
            number: !0,
            password: !0,
            range: !0,
            search: !0,
            tel: !0,
            text: !0,
            time: !0,
            url: !0,
            week: !0
        };
        function ih(t) {
            var e = t && t.nodeName && t.nodeName.toLowerCase();
            return e === "input" ? !!Bx[t.type] : e === "textarea";
        }
        function lh(t, e, i, s) {
            Ya ? Xa ? Xa.push(s) : Xa = [
                s
            ] : Ya = s, e = sr(e, "onChange"), 0 < e.length && (i = new gs("onChange", "change", null, i, s), t.push({
                event: i,
                listeners: e
            }));
        }
        var Pi = null, Zi = null;
        function Ux(t) {
            qp(t, 0);
        }
        function vs(t) {
            var e = Gi(t);
            if (Gd(e)) return t;
        }
        function sh(t, e) {
            if (t === "change") return e;
        }
        var rh = !1;
        if (dn) {
            var Do;
            if (dn) {
                var jo = "oninput" in document;
                if (!jo) {
                    var oh = document.createElement("div");
                    oh.setAttribute("oninput", "return;"), jo = typeof oh.oninput == "function";
                }
                Do = jo;
            } else Do = !1;
            rh = Do && (!document.documentMode || 9 < document.documentMode);
        }
        function uh() {
            Pi && (Pi.detachEvent("onpropertychange", ch), Zi = Pi = null);
        }
        function ch(t) {
            if (t.propertyName === "value" && vs(Zi)) {
                var e = [];
                lh(e, Zi, t, So(t)), Zd(Ux, e);
            }
        }
        function Hx(t, e, i) {
            t === "focusin" ? (uh(), Pi = e, Zi = i, Pi.attachEvent("onpropertychange", ch)) : t === "focusout" && uh();
        }
        function Gx(t) {
            if (t === "selectionchange" || t === "keyup" || t === "keydown") return vs(Zi);
        }
        function qx(t, e) {
            if (t === "click") return vs(e);
        }
        function kx(t, e) {
            if (t === "input" || t === "change") return vs(e);
        }
        function Yx(t, e) {
            return t === e && (t !== 0 || 1 / t === 1 / e) || t !== t && e !== e;
        }
        var _e = typeof Object.is == "function" ? Object.is : Yx;
        function Qi(t, e) {
            if (_e(t, e)) return !0;
            if (typeof t != "object" || t === null || typeof e != "object" || e === null) return !1;
            var i = Object.keys(t), s = Object.keys(e);
            if (i.length !== s.length) return !1;
            for(s = 0; s < i.length; s++){
                var o = i[s];
                if (!ro.call(e, o) || !_e(t[o], e[o])) return !1;
            }
            return !0;
        }
        function fh(t) {
            for(; t && t.firstChild;)t = t.firstChild;
            return t;
        }
        function dh(t, e) {
            var i = fh(t);
            t = 0;
            for(var s; i;){
                if (i.nodeType === 3) {
                    if (s = t + i.textContent.length, t <= e && s >= e) return {
                        node: i,
                        offset: e - t
                    };
                    t = s;
                }
                t: {
                    for(; i;){
                        if (i.nextSibling) {
                            i = i.nextSibling;
                            break t;
                        }
                        i = i.parentNode;
                    }
                    i = void 0;
                }
                i = fh(i);
            }
        }
        function hh(t, e) {
            return t && e ? t === e ? !0 : t && t.nodeType === 3 ? !1 : e && e.nodeType === 3 ? hh(t, e.parentNode) : "contains" in t ? t.contains(e) : t.compareDocumentPosition ? !!(t.compareDocumentPosition(e) & 16) : !1 : !1;
        }
        function mh(t) {
            t = t != null && t.ownerDocument != null && t.ownerDocument.defaultView != null ? t.ownerDocument.defaultView : window;
            for(var e = fs(t.document); e instanceof t.HTMLIFrameElement;){
                try {
                    var i = typeof e.contentWindow.location.href == "string";
                } catch  {
                    i = !1;
                }
                if (i) t = e.contentWindow;
                else break;
                e = fs(t.document);
            }
            return e;
        }
        function Oo(t) {
            var e = t && t.nodeName && t.nodeName.toLowerCase();
            return e && (e === "input" && (t.type === "text" || t.type === "search" || t.type === "tel" || t.type === "url" || t.type === "password") || e === "textarea" || t.contentEditable === "true");
        }
        var Xx = dn && "documentMode" in document && 11 >= document.documentMode, Pa = null, No = null, Fi = null, zo = !1;
        function ph(t, e, i) {
            var s = i.window === i ? i.document : i.nodeType === 9 ? i : i.ownerDocument;
            zo || Pa == null || Pa !== fs(s) || (s = Pa, "selectionStart" in s && Oo(s) ? s = {
                start: s.selectionStart,
                end: s.selectionEnd
            } : (s = (s.ownerDocument && s.ownerDocument.defaultView || window).getSelection(), s = {
                anchorNode: s.anchorNode,
                anchorOffset: s.anchorOffset,
                focusNode: s.focusNode,
                focusOffset: s.focusOffset
            }), Fi && Qi(Fi, s) || (Fi = s, s = sr(No, "onSelect"), 0 < s.length && (e = new gs("onSelect", "select", null, e, i), t.push({
                event: e,
                listeners: s
            }), e.target = Pa)));
        }
        function pa(t, e) {
            var i = {};
            return i[t.toLowerCase()] = e.toLowerCase(), i["Webkit" + t] = "webkit" + e, i["Moz" + t] = "moz" + e, i;
        }
        var Za = {
            animationend: pa("Animation", "AnimationEnd"),
            animationiteration: pa("Animation", "AnimationIteration"),
            animationstart: pa("Animation", "AnimationStart"),
            transitionrun: pa("Transition", "TransitionRun"),
            transitionstart: pa("Transition", "TransitionStart"),
            transitioncancel: pa("Transition", "TransitionCancel"),
            transitionend: pa("Transition", "TransitionEnd")
        }, Lo = {}, gh = {};
        dn && (gh = document.createElement("div").style, "AnimationEvent" in window || (delete Za.animationend.animation, delete Za.animationiteration.animation, delete Za.animationstart.animation), "TransitionEvent" in window || delete Za.transitionend.transition);
        function ga(t) {
            if (Lo[t]) return Lo[t];
            if (!Za[t]) return t;
            var e = Za[t], i;
            for(i in e)if (e.hasOwnProperty(i) && i in gh) return Lo[t] = e[i];
            return t;
        }
        var yh = ga("animationend"), vh = ga("animationiteration"), bh = ga("animationstart"), Kx = ga("transitionrun"), Px = ga("transitionstart"), Zx = ga("transitioncancel"), xh = ga("transitionend"), Sh = new Map, Vo = "abort auxClick beforeToggle cancel canPlay canPlayThrough click close contextMenu copy cut drag dragEnd dragEnter dragExit dragLeave dragOver dragStart drop durationChange emptied encrypted ended error gotPointerCapture input invalid keyDown keyPress keyUp load loadedData loadedMetadata loadStart lostPointerCapture mouseDown mouseMove mouseOut mouseOver mouseUp paste pause play playing pointerCancel pointerDown pointerMove pointerOut pointerOver pointerUp progress rateChange reset resize seeked seeking stalled submit suspend timeUpdate touchCancel touchEnd touchStart volumeChange scroll toggle touchMove waiting wheel".split(" ");
        Vo.push("scrollEnd");
        function Qe(t, e) {
            Sh.set(t, e), ha(e, [
                t
            ]);
        }
        var bs = typeof reportError == "function" ? reportError : function(t) {
            if (typeof window == "object" && typeof window.ErrorEvent == "function") {
                var e = new window.ErrorEvent("error", {
                    bubbles: !0,
                    cancelable: !0,
                    message: typeof t == "object" && t !== null && typeof t.message == "string" ? String(t.message) : String(t),
                    error: t
                });
                if (!window.dispatchEvent(e)) return;
            } else if (typeof process == "object" && typeof process.emit == "function") {
                process.emit("uncaughtException", t);
                return;
            }
            console.error(t);
        }, Ve = [], Qa = 0, Bo = 0;
        function xs() {
            for(var t = Qa, e = Bo = Qa = 0; e < t;){
                var i = Ve[e];
                Ve[e++] = null;
                var s = Ve[e];
                Ve[e++] = null;
                var o = Ve[e];
                Ve[e++] = null;
                var c = Ve[e];
                if (Ve[e++] = null, s !== null && o !== null) {
                    var g = s.pending;
                    g === null ? o.next = o : (o.next = g.next, g.next = o), s.pending = o;
                }
                c !== 0 && Th(i, o, c);
            }
        }
        function Ss(t, e, i, s) {
            Ve[Qa++] = t, Ve[Qa++] = e, Ve[Qa++] = i, Ve[Qa++] = s, Bo |= s, t.lanes |= s, t = t.alternate, t !== null && (t.lanes |= s);
        }
        function Uo(t, e, i, s) {
            return Ss(t, e, i, s), Ts(t);
        }
        function ya(t, e) {
            return Ss(t, null, null, e), Ts(t);
        }
        function Th(t, e, i) {
            t.lanes |= i;
            var s = t.alternate;
            s !== null && (s.lanes |= i);
            for(var o = !1, c = t.return; c !== null;)c.childLanes |= i, s = c.alternate, s !== null && (s.childLanes |= i), c.tag === 22 && (t = c.stateNode, t === null || t._visibility & 1 || (o = !0)), t = c, c = c.return;
            return t.tag === 3 ? (c = t.stateNode, o && e !== null && (o = 31 - we(i), t = c.hiddenUpdates, s = t[o], s === null ? t[o] = [
                e
            ] : s.push(e), e.lane = i | 536870912), c) : null;
        }
        function Ts(t) {
            if (50 < yl) throw yl = 0, Zu = null, Error(r(185));
            for(var e = t.return; e !== null;)t = e, e = t.return;
            return t.tag === 3 ? t.stateNode : null;
        }
        var Fa = {};
        function Qx(t, e, i, s) {
            this.tag = t, this.key = i, this.sibling = this.child = this.return = this.stateNode = this.type = this.elementType = null, this.index = 0, this.refCleanup = this.ref = null, this.pendingProps = e, this.dependencies = this.memoizedState = this.updateQueue = this.memoizedProps = null, this.mode = s, this.subtreeFlags = this.flags = 0, this.deletions = null, this.childLanes = this.lanes = 0, this.alternate = null;
        }
        function Re(t, e, i, s) {
            return new Qx(t, e, i, s);
        }
        function Ho(t) {
            return t = t.prototype, !(!t || !t.isReactComponent);
        }
        function hn(t, e) {
            var i = t.alternate;
            return i === null ? (i = Re(t.tag, e, t.key, t.mode), i.elementType = t.elementType, i.type = t.type, i.stateNode = t.stateNode, i.alternate = t, t.alternate = i) : (i.pendingProps = e, i.type = t.type, i.flags = 0, i.subtreeFlags = 0, i.deletions = null), i.flags = t.flags & 65011712, i.childLanes = t.childLanes, i.lanes = t.lanes, i.child = t.child, i.memoizedProps = t.memoizedProps, i.memoizedState = t.memoizedState, i.updateQueue = t.updateQueue, e = t.dependencies, i.dependencies = e === null ? null : {
                lanes: e.lanes,
                firstContext: e.firstContext
            }, i.sibling = t.sibling, i.index = t.index, i.ref = t.ref, i.refCleanup = t.refCleanup, i;
        }
        function Eh(t, e) {
            t.flags &= 65011714;
            var i = t.alternate;
            return i === null ? (t.childLanes = 0, t.lanes = e, t.child = null, t.subtreeFlags = 0, t.memoizedProps = null, t.memoizedState = null, t.updateQueue = null, t.dependencies = null, t.stateNode = null) : (t.childLanes = i.childLanes, t.lanes = i.lanes, t.child = i.child, t.subtreeFlags = 0, t.deletions = null, t.memoizedProps = i.memoizedProps, t.memoizedState = i.memoizedState, t.updateQueue = i.updateQueue, t.type = i.type, e = i.dependencies, t.dependencies = e === null ? null : {
                lanes: e.lanes,
                firstContext: e.firstContext
            }), t;
        }
        function Es(t, e, i, s, o, c) {
            var g = 0;
            if (s = t, typeof t == "function") Ho(t) && (g = 1);
            else if (typeof t == "string") g = IS(t, i, W.current) ? 26 : t === "html" || t === "head" || t === "body" ? 27 : 5;
            else t: switch(t){
                case Q:
                    return t = Re(31, i, e, o), t.elementType = Q, t.lanes = c, t;
                case M:
                    return va(i.children, o, c, e);
                case R:
                    g = 8, o |= 24;
                    break;
                case z:
                    return t = Re(12, i, e, o | 2), t.elementType = z, t.lanes = c, t;
                case U:
                    return t = Re(13, i, e, o), t.elementType = U, t.lanes = c, t;
                case X:
                    return t = Re(19, i, e, o), t.elementType = X, t.lanes = c, t;
                default:
                    if (typeof t == "object" && t !== null) switch(t.$$typeof){
                        case V:
                            g = 10;
                            break t;
                        case B:
                            g = 9;
                            break t;
                        case P:
                            g = 11;
                            break t;
                        case H:
                            g = 14;
                            break t;
                        case Z:
                            g = 16, s = null;
                            break t;
                    }
                    g = 29, i = Error(r(130, t === null ? "null" : typeof t, "")), s = null;
            }
            return e = Re(g, i, e, o), e.elementType = t, e.type = s, e.lanes = c, e;
        }
        function va(t, e, i, s) {
            return t = Re(7, t, s, e), t.lanes = i, t;
        }
        function Go(t, e, i) {
            return t = Re(6, t, null, e), t.lanes = i, t;
        }
        function Ah(t) {
            var e = Re(18, null, null, 0);
            return e.stateNode = t, e;
        }
        function qo(t, e, i) {
            return e = Re(4, t.children !== null ? t.children : [], t.key, e), e.lanes = i, e.stateNode = {
                containerInfo: t.containerInfo,
                pendingChildren: null,
                implementation: t.implementation
            }, e;
        }
        var Ch = new WeakMap;
        function Be(t, e) {
            if (typeof t == "object" && t !== null) {
                var i = Ch.get(t);
                return i !== void 0 ? i : (e = {
                    value: t,
                    source: e,
                    stack: Ad(e)
                }, Ch.set(t, e), e);
            }
            return {
                value: t,
                source: e,
                stack: Ad(e)
            };
        }
        var $a = [], Ja = 0, As = null, $i = 0, Ue = [], He = 0, Bn = null, tn = 1, en = "";
        function mn(t, e) {
            $a[Ja++] = $i, $a[Ja++] = As, As = t, $i = e;
        }
        function wh(t, e, i) {
            Ue[He++] = tn, Ue[He++] = en, Ue[He++] = Bn, Bn = t;
            var s = tn;
            t = en;
            var o = 32 - we(s) - 1;
            s &= ~(1 << o), i += 1;
            var c = 32 - we(e) + o;
            if (30 < c) {
                var g = o - o % 5;
                c = (s & (1 << g) - 1).toString(32), s >>= g, o -= g, tn = 1 << 32 - we(e) + o | i << o | s, en = c + t;
            } else tn = 1 << c | i << o | s, en = t;
        }
        function ko(t) {
            t.return !== null && (mn(t, 1), wh(t, 1, 0));
        }
        function Yo(t) {
            for(; t === As;)As = $a[--Ja], $a[Ja] = null, $i = $a[--Ja], $a[Ja] = null;
            for(; t === Bn;)Bn = Ue[--He], Ue[He] = null, en = Ue[--He], Ue[He] = null, tn = Ue[--He], Ue[He] = null;
        }
        function _h(t, e) {
            Ue[He++] = tn, Ue[He++] = en, Ue[He++] = Bn, tn = e.id, en = e.overflow, Bn = t;
        }
        var se = null, zt = null, vt = !1, Un = null, Ge = !1, Xo = Error(r(519));
        function Hn(t) {
            var e = Error(r(418, 1 < arguments.length && arguments[1] !== void 0 && arguments[1] ? "text" : "HTML", ""));
            throw Ji(Be(e, t)), Xo;
        }
        function Rh(t) {
            var e = t.stateNode, i = t.type, s = t.memoizedProps;
            switch(e[le] = t, e[ye] = s, i){
                case "dialog":
                    ht("cancel", e), ht("close", e);
                    break;
                case "iframe":
                case "object":
                case "embed":
                    ht("load", e);
                    break;
                case "video":
                case "audio":
                    for(i = 0; i < bl.length; i++)ht(bl[i], e);
                    break;
                case "source":
                    ht("error", e);
                    break;
                case "img":
                case "image":
                case "link":
                    ht("error", e), ht("load", e);
                    break;
                case "details":
                    ht("toggle", e);
                    break;
                case "input":
                    ht("invalid", e), qd(e, s.value, s.defaultValue, s.checked, s.defaultChecked, s.type, s.name, !0);
                    break;
                case "select":
                    ht("invalid", e);
                    break;
                case "textarea":
                    ht("invalid", e), Yd(e, s.value, s.defaultValue, s.children);
            }
            i = s.children, typeof i != "string" && typeof i != "number" && typeof i != "bigint" || e.textContent === "" + i || s.suppressHydrationWarning === !0 || Kp(e.textContent, i) ? (s.popover != null && (ht("beforetoggle", e), ht("toggle", e)), s.onScroll != null && ht("scroll", e), s.onScrollEnd != null && ht("scrollend", e), s.onClick != null && (e.onclick = fn), e = !0) : e = !1, e || Hn(t, !0);
        }
        function Mh(t) {
            for(se = t.return; se;)switch(se.tag){
                case 5:
                case 31:
                case 13:
                    Ge = !1;
                    return;
                case 27:
                case 3:
                    Ge = !0;
                    return;
                default:
                    se = se.return;
            }
        }
        function Wa(t) {
            if (t !== se) return !1;
            if (!vt) return Mh(t), vt = !0, !1;
            var e = t.tag, i;
            if ((i = e !== 3 && e !== 27) && ((i = e === 5) && (i = t.type, i = !(i !== "form" && i !== "button") || oc(t.type, t.memoizedProps)), i = !i), i && zt && Hn(t), Mh(t), e === 13) {
                if (t = t.memoizedState, t = t !== null ? t.dehydrated : null, !t) throw Error(r(317));
                zt = tg(t);
            } else if (e === 31) {
                if (t = t.memoizedState, t = t !== null ? t.dehydrated : null, !t) throw Error(r(317));
                zt = tg(t);
            } else e === 27 ? (e = zt, In(t.type) ? (t = hc, hc = null, zt = t) : zt = e) : zt = se ? ke(t.stateNode.nextSibling) : null;
            return !0;
        }
        function ba() {
            zt = se = null, vt = !1;
        }
        function Ko() {
            var t = Un;
            return t !== null && (Te === null ? Te = t : Te.push.apply(Te, t), Un = null), t;
        }
        function Ji(t) {
            Un === null ? Un = [
                t
            ] : Un.push(t);
        }
        var Po = w(null), xa = null, pn = null;
        function Gn(t, e, i) {
            J(Po, e._currentValue), e._currentValue = i;
        }
        function gn(t) {
            t._currentValue = Po.current, Y(Po);
        }
        function Zo(t, e, i) {
            for(; t !== null;){
                var s = t.alternate;
                if ((t.childLanes & e) !== e ? (t.childLanes |= e, s !== null && (s.childLanes |= e)) : s !== null && (s.childLanes & e) !== e && (s.childLanes |= e), t === i) break;
                t = t.return;
            }
        }
        function Qo(t, e, i, s) {
            var o = t.child;
            for(o !== null && (o.return = t); o !== null;){
                var c = o.dependencies;
                if (c !== null) {
                    var g = o.child;
                    c = c.firstContext;
                    t: for(; c !== null;){
                        var b = c;
                        c = o;
                        for(var C = 0; C < e.length; C++)if (b.context === e[C]) {
                            c.lanes |= i, b = c.alternate, b !== null && (b.lanes |= i), Zo(c.return, i, t), s || (g = null);
                            break t;
                        }
                        c = b.next;
                    }
                } else if (o.tag === 18) {
                    if (g = o.return, g === null) throw Error(r(341));
                    g.lanes |= i, c = g.alternate, c !== null && (c.lanes |= i), Zo(g, i, t), g = null;
                } else g = o.child;
                if (g !== null) g.return = o;
                else for(g = o; g !== null;){
                    if (g === t) {
                        g = null;
                        break;
                    }
                    if (o = g.sibling, o !== null) {
                        o.return = g.return, g = o;
                        break;
                    }
                    g = g.return;
                }
                o = g;
            }
        }
        function Ia(t, e, i, s) {
            t = null;
            for(var o = e, c = !1; o !== null;){
                if (!c) {
                    if ((o.flags & 524288) !== 0) c = !0;
                    else if ((o.flags & 262144) !== 0) break;
                }
                if (o.tag === 10) {
                    var g = o.alternate;
                    if (g === null) throw Error(r(387));
                    if (g = g.memoizedProps, g !== null) {
                        var b = o.type;
                        _e(o.pendingProps.value, g.value) || (t !== null ? t.push(b) : t = [
                            b
                        ]);
                    }
                } else if (o === yt.current) {
                    if (g = o.alternate, g === null) throw Error(r(387));
                    g.memoizedState.memoizedState !== o.memoizedState.memoizedState && (t !== null ? t.push(Al) : t = [
                        Al
                    ]);
                }
                o = o.return;
            }
            t !== null && Qo(e, t, i, s), e.flags |= 262144;
        }
        function Cs(t) {
            for(t = t.firstContext; t !== null;){
                if (!_e(t.context._currentValue, t.memoizedValue)) return !0;
                t = t.next;
            }
            return !1;
        }
        function Sa(t) {
            xa = t, pn = null, t = t.dependencies, t !== null && (t.firstContext = null);
        }
        function re(t) {
            return Dh(xa, t);
        }
        function ws(t, e) {
            return xa === null && Sa(t), Dh(t, e);
        }
        function Dh(t, e) {
            var i = e._currentValue;
            if (e = {
                context: e,
                memoizedValue: i,
                next: null
            }, pn === null) {
                if (t === null) throw Error(r(308));
                pn = e, t.dependencies = {
                    lanes: 0,
                    firstContext: e
                }, t.flags |= 524288;
            } else pn = pn.next = e;
            return i;
        }
        var Fx = typeof AbortController < "u" ? AbortController : function() {
            var t = [], e = this.signal = {
                aborted: !1,
                addEventListener: function(i, s) {
                    t.push(s);
                }
            };
            this.abort = function() {
                e.aborted = !0, t.forEach(function(i) {
                    return i();
                });
            };
        }, $x = n.unstable_scheduleCallback, Jx = n.unstable_NormalPriority, Zt = {
            $$typeof: V,
            Consumer: null,
            Provider: null,
            _currentValue: null,
            _currentValue2: null,
            _threadCount: 0
        };
        function Fo() {
            return {
                controller: new Fx,
                data: new Map,
                refCount: 0
            };
        }
        function Wi(t) {
            t.refCount--, t.refCount === 0 && $x(Jx, function() {
                t.controller.abort();
            });
        }
        var Ii = null, $o = 0, ti = 0, ei = null;
        function Wx(t, e) {
            if (Ii === null) {
                var i = Ii = [];
                $o = 0, ti = Iu(), ei = {
                    status: "pending",
                    value: void 0,
                    then: function(s) {
                        i.push(s);
                    }
                };
            }
            return $o++, e.then(jh, jh), e;
        }
        function jh() {
            if (--$o === 0 && Ii !== null) {
                ei !== null && (ei.status = "fulfilled");
                var t = Ii;
                Ii = null, ti = 0, ei = null;
                for(var e = 0; e < t.length; e++)(0, t[e])();
            }
        }
        function Ix(t, e) {
            var i = [], s = {
                status: "pending",
                value: null,
                reason: null,
                then: function(o) {
                    i.push(o);
                }
            };
            return t.then(function() {
                s.status = "fulfilled", s.value = e;
                for(var o = 0; o < i.length; o++)(0, i[o])(e);
            }, function(o) {
                for(s.status = "rejected", s.reason = o, o = 0; o < i.length; o++)(0, i[o])(void 0);
            }), s;
        }
        var Oh = G.S;
        G.S = function(t, e) {
            pp = Ae(), typeof e == "object" && e !== null && typeof e.then == "function" && Wx(t, e), Oh !== null && Oh(t, e);
        };
        var Ta = w(null);
        function Jo() {
            var t = Ta.current;
            return t !== null ? t : jt.pooledCache;
        }
        function _s(t, e) {
            e === null ? J(Ta, Ta.current) : J(Ta, e.pool);
        }
        function Nh() {
            var t = Jo();
            return t === null ? null : {
                parent: Zt._currentValue,
                pool: t
            };
        }
        var ni = Error(r(460)), Wo = Error(r(474)), Rs = Error(r(542)), Ms = {
            then: function() {}
        };
        function zh(t) {
            return t = t.status, t === "fulfilled" || t === "rejected";
        }
        function Lh(t, e, i) {
            switch(i = t[i], i === void 0 ? t.push(e) : i !== e && (e.then(fn, fn), e = i), e.status){
                case "fulfilled":
                    return e.value;
                case "rejected":
                    throw t = e.reason, Bh(t), t;
                default:
                    if (typeof e.status == "string") e.then(fn, fn);
                    else {
                        if (t = jt, t !== null && 100 < t.shellSuspendCounter) throw Error(r(482));
                        t = e, t.status = "pending", t.then(function(s) {
                            if (e.status === "pending") {
                                var o = e;
                                o.status = "fulfilled", o.value = s;
                            }
                        }, function(s) {
                            if (e.status === "pending") {
                                var o = e;
                                o.status = "rejected", o.reason = s;
                            }
                        });
                    }
                    switch(e.status){
                        case "fulfilled":
                            return e.value;
                        case "rejected":
                            throw t = e.reason, Bh(t), t;
                    }
                    throw Aa = e, ni;
            }
        }
        function Ea(t) {
            try {
                var e = t._init;
                return e(t._payload);
            } catch (i) {
                throw i !== null && typeof i == "object" && typeof i.then == "function" ? (Aa = i, ni) : i;
            }
        }
        var Aa = null;
        function Vh() {
            if (Aa === null) throw Error(r(459));
            var t = Aa;
            return Aa = null, t;
        }
        function Bh(t) {
            if (t === ni || t === Rs) throw Error(r(483));
        }
        var ai = null, tl = 0;
        function Ds(t) {
            var e = tl;
            return tl += 1, ai === null && (ai = []), Lh(ai, t, e);
        }
        function el(t, e) {
            e = e.props.ref, t.ref = e !== void 0 ? e : null;
        }
        function js(t, e) {
            throw e.$$typeof === x ? Error(r(525)) : (t = Object.prototype.toString.call(e), Error(r(31, t === "[object Object]" ? "object with keys {" + Object.keys(e).join(", ") + "}" : t)));
        }
        function Uh(t) {
            function e(D, _) {
                if (t) {
                    var j = D.deletions;
                    j === null ? (D.deletions = [
                        _
                    ], D.flags |= 16) : j.push(_);
                }
            }
            function i(D, _) {
                if (!t) return null;
                for(; _ !== null;)e(D, _), _ = _.sibling;
                return null;
            }
            function s(D) {
                for(var _ = new Map; D !== null;)D.key !== null ? _.set(D.key, D) : _.set(D.index, D), D = D.sibling;
                return _;
            }
            function o(D, _) {
                return D = hn(D, _), D.index = 0, D.sibling = null, D;
            }
            function c(D, _, j) {
                return D.index = j, t ? (j = D.alternate, j !== null ? (j = j.index, j < _ ? (D.flags |= 67108866, _) : j) : (D.flags |= 67108866, _)) : (D.flags |= 1048576, _);
            }
            function g(D) {
                return t && D.alternate === null && (D.flags |= 67108866), D;
            }
            function b(D, _, j, k) {
                return _ === null || _.tag !== 6 ? (_ = Go(j, D.mode, k), _.return = D, _) : (_ = o(_, j), _.return = D, _);
            }
            function C(D, _, j, k) {
                var nt = j.type;
                return nt === M ? q(D, _, j.props.children, k, j.key) : _ !== null && (_.elementType === nt || typeof nt == "object" && nt !== null && nt.$$typeof === Z && Ea(nt) === _.type) ? (_ = o(_, j.props), el(_, j), _.return = D, _) : (_ = Es(j.type, j.key, j.props, null, D.mode, k), el(_, j), _.return = D, _);
            }
            function O(D, _, j, k) {
                return _ === null || _.tag !== 4 || _.stateNode.containerInfo !== j.containerInfo || _.stateNode.implementation !== j.implementation ? (_ = qo(j, D.mode, k), _.return = D, _) : (_ = o(_, j.children || []), _.return = D, _);
            }
            function q(D, _, j, k, nt) {
                return _ === null || _.tag !== 7 ? (_ = va(j, D.mode, k, nt), _.return = D, _) : (_ = o(_, j), _.return = D, _);
            }
            function K(D, _, j) {
                if (typeof _ == "string" && _ !== "" || typeof _ == "number" || typeof _ == "bigint") return _ = Go("" + _, D.mode, j), _.return = D, _;
                if (typeof _ == "object" && _ !== null) {
                    switch(_.$$typeof){
                        case A:
                            return j = Es(_.type, _.key, _.props, null, D.mode, j), el(j, _), j.return = D, j;
                        case E:
                            return _ = qo(_, D.mode, j), _.return = D, _;
                        case Z:
                            return _ = Ea(_), K(D, _, j);
                    }
                    if (Vt(_) || gt(_)) return _ = va(_, D.mode, j, null), _.return = D, _;
                    if (typeof _.then == "function") return K(D, Ds(_), j);
                    if (_.$$typeof === V) return K(D, ws(D, _), j);
                    js(D, _);
                }
                return null;
            }
            function N(D, _, j, k) {
                var nt = _ !== null ? _.key : null;
                if (typeof j == "string" && j !== "" || typeof j == "number" || typeof j == "bigint") return nt !== null ? null : b(D, _, "" + j, k);
                if (typeof j == "object" && j !== null) {
                    switch(j.$$typeof){
                        case A:
                            return j.key === nt ? C(D, _, j, k) : null;
                        case E:
                            return j.key === nt ? O(D, _, j, k) : null;
                        case Z:
                            return j = Ea(j), N(D, _, j, k);
                    }
                    if (Vt(j) || gt(j)) return nt !== null ? null : q(D, _, j, k, null);
                    if (typeof j.then == "function") return N(D, _, Ds(j), k);
                    if (j.$$typeof === V) return N(D, _, ws(D, j), k);
                    js(D, j);
                }
                return null;
            }
            function L(D, _, j, k, nt) {
                if (typeof k == "string" && k !== "" || typeof k == "number" || typeof k == "bigint") return D = D.get(j) || null, b(_, D, "" + k, nt);
                if (typeof k == "object" && k !== null) {
                    switch(k.$$typeof){
                        case A:
                            return D = D.get(k.key === null ? j : k.key) || null, C(_, D, k, nt);
                        case E:
                            return D = D.get(k.key === null ? j : k.key) || null, O(_, D, k, nt);
                        case Z:
                            return k = Ea(k), L(D, _, j, k, nt);
                    }
                    if (Vt(k) || gt(k)) return D = D.get(j) || null, q(_, D, k, nt, null);
                    if (typeof k.then == "function") return L(D, _, j, Ds(k), nt);
                    if (k.$$typeof === V) return L(D, _, j, ws(_, k), nt);
                    js(_, k);
                }
                return null;
            }
            function I(D, _, j, k) {
                for(var nt = null, xt = null, et = _, ct = _ = 0, pt = null; et !== null && ct < j.length; ct++){
                    et.index > ct ? (pt = et, et = null) : pt = et.sibling;
                    var St = N(D, et, j[ct], k);
                    if (St === null) {
                        et === null && (et = pt);
                        break;
                    }
                    t && et && St.alternate === null && e(D, et), _ = c(St, _, ct), xt === null ? nt = St : xt.sibling = St, xt = St, et = pt;
                }
                if (ct === j.length) return i(D, et), vt && mn(D, ct), nt;
                if (et === null) {
                    for(; ct < j.length; ct++)et = K(D, j[ct], k), et !== null && (_ = c(et, _, ct), xt === null ? nt = et : xt.sibling = et, xt = et);
                    return vt && mn(D, ct), nt;
                }
                for(et = s(et); ct < j.length; ct++)pt = L(et, D, ct, j[ct], k), pt !== null && (t && pt.alternate !== null && et.delete(pt.key === null ? ct : pt.key), _ = c(pt, _, ct), xt === null ? nt = pt : xt.sibling = pt, xt = pt);
                return t && et.forEach(function(ia) {
                    return e(D, ia);
                }), vt && mn(D, ct), nt;
            }
            function at(D, _, j, k) {
                if (j == null) throw Error(r(151));
                for(var nt = null, xt = null, et = _, ct = _ = 0, pt = null, St = j.next(); et !== null && !St.done; ct++, St = j.next()){
                    et.index > ct ? (pt = et, et = null) : pt = et.sibling;
                    var ia = N(D, et, St.value, k);
                    if (ia === null) {
                        et === null && (et = pt);
                        break;
                    }
                    t && et && ia.alternate === null && e(D, et), _ = c(ia, _, ct), xt === null ? nt = ia : xt.sibling = ia, xt = ia, et = pt;
                }
                if (St.done) return i(D, et), vt && mn(D, ct), nt;
                if (et === null) {
                    for(; !St.done; ct++, St = j.next())St = K(D, St.value, k), St !== null && (_ = c(St, _, ct), xt === null ? nt = St : xt.sibling = St, xt = St);
                    return vt && mn(D, ct), nt;
                }
                for(et = s(et); !St.done; ct++, St = j.next())St = L(et, D, ct, St.value, k), St !== null && (t && St.alternate !== null && et.delete(St.key === null ? ct : St.key), _ = c(St, _, ct), xt === null ? nt = St : xt.sibling = St, xt = St);
                return t && et.forEach(function(c1) {
                    return e(D, c1);
                }), vt && mn(D, ct), nt;
            }
            function Mt(D, _, j, k) {
                if (typeof j == "object" && j !== null && j.type === M && j.key === null && (j = j.props.children), typeof j == "object" && j !== null) {
                    switch(j.$$typeof){
                        case A:
                            t: {
                                for(var nt = j.key; _ !== null;){
                                    if (_.key === nt) {
                                        if (nt = j.type, nt === M) {
                                            if (_.tag === 7) {
                                                i(D, _.sibling), k = o(_, j.props.children), k.return = D, D = k;
                                                break t;
                                            }
                                        } else if (_.elementType === nt || typeof nt == "object" && nt !== null && nt.$$typeof === Z && Ea(nt) === _.type) {
                                            i(D, _.sibling), k = o(_, j.props), el(k, j), k.return = D, D = k;
                                            break t;
                                        }
                                        i(D, _);
                                        break;
                                    } else e(D, _);
                                    _ = _.sibling;
                                }
                                j.type === M ? (k = va(j.props.children, D.mode, k, j.key), k.return = D, D = k) : (k = Es(j.type, j.key, j.props, null, D.mode, k), el(k, j), k.return = D, D = k);
                            }
                            return g(D);
                        case E:
                            t: {
                                for(nt = j.key; _ !== null;){
                                    if (_.key === nt) if (_.tag === 4 && _.stateNode.containerInfo === j.containerInfo && _.stateNode.implementation === j.implementation) {
                                        i(D, _.sibling), k = o(_, j.children || []), k.return = D, D = k;
                                        break t;
                                    } else {
                                        i(D, _);
                                        break;
                                    }
                                    else e(D, _);
                                    _ = _.sibling;
                                }
                                k = qo(j, D.mode, k), k.return = D, D = k;
                            }
                            return g(D);
                        case Z:
                            return j = Ea(j), Mt(D, _, j, k);
                    }
                    if (Vt(j)) return I(D, _, j, k);
                    if (gt(j)) {
                        if (nt = gt(j), typeof nt != "function") throw Error(r(150));
                        return j = nt.call(j), at(D, _, j, k);
                    }
                    if (typeof j.then == "function") return Mt(D, _, Ds(j), k);
                    if (j.$$typeof === V) return Mt(D, _, ws(D, j), k);
                    js(D, j);
                }
                return typeof j == "string" && j !== "" || typeof j == "number" || typeof j == "bigint" ? (j = "" + j, _ !== null && _.tag === 6 ? (i(D, _.sibling), k = o(_, j), k.return = D, D = k) : (i(D, _), k = Go(j, D.mode, k), k.return = D, D = k), g(D)) : i(D, _);
            }
            return function(D, _, j, k) {
                try {
                    tl = 0;
                    var nt = Mt(D, _, j, k);
                    return ai = null, nt;
                } catch (et) {
                    if (et === ni || et === Rs) throw et;
                    var xt = Re(29, et, null, D.mode);
                    return xt.lanes = k, xt.return = D, xt;
                } finally{}
            };
        }
        var Ca = Uh(!0), Hh = Uh(!1), qn = !1;
        function Io(t) {
            t.updateQueue = {
                baseState: t.memoizedState,
                firstBaseUpdate: null,
                lastBaseUpdate: null,
                shared: {
                    pending: null,
                    lanes: 0,
                    hiddenCallbacks: null
                },
                callbacks: null
            };
        }
        function tu(t, e) {
            t = t.updateQueue, e.updateQueue === t && (e.updateQueue = {
                baseState: t.baseState,
                firstBaseUpdate: t.firstBaseUpdate,
                lastBaseUpdate: t.lastBaseUpdate,
                shared: t.shared,
                callbacks: null
            });
        }
        function kn(t) {
            return {
                lane: t,
                tag: 0,
                payload: null,
                callback: null,
                next: null
            };
        }
        function Yn(t, e, i) {
            var s = t.updateQueue;
            if (s === null) return null;
            if (s = s.shared, (Et & 2) !== 0) {
                var o = s.pending;
                return o === null ? e.next = e : (e.next = o.next, o.next = e), s.pending = e, e = Ts(t), Th(t, null, i), e;
            }
            return Ss(t, s, e, i), Ts(t);
        }
        function nl(t, e, i) {
            if (e = e.updateQueue, e !== null && (e = e.shared, (i & 4194048) !== 0)) {
                var s = e.lanes;
                s &= t.pendingLanes, i |= s, e.lanes = i, Dd(t, i);
            }
        }
        function eu(t, e) {
            var i = t.updateQueue, s = t.alternate;
            if (s !== null && (s = s.updateQueue, i === s)) {
                var o = null, c = null;
                if (i = i.firstBaseUpdate, i !== null) {
                    do {
                        var g = {
                            lane: i.lane,
                            tag: i.tag,
                            payload: i.payload,
                            callback: null,
                            next: null
                        };
                        c === null ? o = c = g : c = c.next = g, i = i.next;
                    }while (i !== null);
                    c === null ? o = c = e : c = c.next = e;
                } else o = c = e;
                i = {
                    baseState: s.baseState,
                    firstBaseUpdate: o,
                    lastBaseUpdate: c,
                    shared: s.shared,
                    callbacks: s.callbacks
                }, t.updateQueue = i;
                return;
            }
            t = i.lastBaseUpdate, t === null ? i.firstBaseUpdate = e : t.next = e, i.lastBaseUpdate = e;
        }
        var nu = !1;
        function al() {
            if (nu) {
                var t = ei;
                if (t !== null) throw t;
            }
        }
        function il(t, e, i, s) {
            nu = !1;
            var o = t.updateQueue;
            qn = !1;
            var c = o.firstBaseUpdate, g = o.lastBaseUpdate, b = o.shared.pending;
            if (b !== null) {
                o.shared.pending = null;
                var C = b, O = C.next;
                C.next = null, g === null ? c = O : g.next = O, g = C;
                var q = t.alternate;
                q !== null && (q = q.updateQueue, b = q.lastBaseUpdate, b !== g && (b === null ? q.firstBaseUpdate = O : b.next = O, q.lastBaseUpdate = C));
            }
            if (c !== null) {
                var K = o.baseState;
                g = 0, q = O = C = null, b = c;
                do {
                    var N = b.lane & -536870913, L = N !== b.lane;
                    if (L ? (mt & N) === N : (s & N) === N) {
                        N !== 0 && N === ti && (nu = !0), q !== null && (q = q.next = {
                            lane: 0,
                            tag: b.tag,
                            payload: b.payload,
                            callback: null,
                            next: null
                        });
                        t: {
                            var I = t, at = b;
                            N = e;
                            var Mt = i;
                            switch(at.tag){
                                case 1:
                                    if (I = at.payload, typeof I == "function") {
                                        K = I.call(Mt, K, N);
                                        break t;
                                    }
                                    K = I;
                                    break t;
                                case 3:
                                    I.flags = I.flags & -65537 | 128;
                                case 0:
                                    if (I = at.payload, N = typeof I == "function" ? I.call(Mt, K, N) : I, N == null) break t;
                                    K = v({}, K, N);
                                    break t;
                                case 2:
                                    qn = !0;
                            }
                        }
                        N = b.callback, N !== null && (t.flags |= 64, L && (t.flags |= 8192), L = o.callbacks, L === null ? o.callbacks = [
                            N
                        ] : L.push(N));
                    } else L = {
                        lane: N,
                        tag: b.tag,
                        payload: b.payload,
                        callback: b.callback,
                        next: null
                    }, q === null ? (O = q = L, C = K) : q = q.next = L, g |= N;
                    if (b = b.next, b === null) {
                        if (b = o.shared.pending, b === null) break;
                        L = b, b = L.next, L.next = null, o.lastBaseUpdate = L, o.shared.pending = null;
                    }
                }while (!0);
                q === null && (C = K), o.baseState = C, o.firstBaseUpdate = O, o.lastBaseUpdate = q, c === null && (o.shared.lanes = 0), Qn |= g, t.lanes = g, t.memoizedState = K;
            }
        }
        function Gh(t, e) {
            if (typeof t != "function") throw Error(r(191, t));
            t.call(e);
        }
        function qh(t, e) {
            var i = t.callbacks;
            if (i !== null) for(t.callbacks = null, t = 0; t < i.length; t++)Gh(i[t], e);
        }
        var ii = w(null), Os = w(0);
        function kh(t, e) {
            t = Cn, J(Os, t), J(ii, e), Cn = t | e.baseLanes;
        }
        function au() {
            J(Os, Cn), J(ii, ii.current);
        }
        function iu() {
            Cn = Os.current, Y(ii), Y(Os);
        }
        var Me = w(null), qe = null;
        function Xn(t) {
            var e = t.alternate;
            J(Kt, Kt.current & 1), J(Me, t), qe === null && (e === null || ii.current !== null || e.memoizedState !== null) && (qe = t);
        }
        function lu(t) {
            J(Kt, Kt.current), J(Me, t), qe === null && (qe = t);
        }
        function Yh(t) {
            t.tag === 22 ? (J(Kt, Kt.current), J(Me, t), qe === null && (qe = t)) : Kn();
        }
        function Kn() {
            J(Kt, Kt.current), J(Me, Me.current);
        }
        function De(t) {
            Y(Me), qe === t && (qe = null), Y(Kt);
        }
        var Kt = w(0);
        function Ns(t) {
            for(var e = t; e !== null;){
                if (e.tag === 13) {
                    var i = e.memoizedState;
                    if (i !== null && (i = i.dehydrated, i === null || fc(i) || dc(i))) return e;
                } else if (e.tag === 19 && (e.memoizedProps.revealOrder === "forwards" || e.memoizedProps.revealOrder === "backwards" || e.memoizedProps.revealOrder === "unstable_legacy-backwards" || e.memoizedProps.revealOrder === "together")) {
                    if ((e.flags & 128) !== 0) return e;
                } else if (e.child !== null) {
                    e.child.return = e, e = e.child;
                    continue;
                }
                if (e === t) break;
                for(; e.sibling === null;){
                    if (e.return === null || e.return === t) return null;
                    e = e.return;
                }
                e.sibling.return = e.return, e = e.sibling;
            }
            return null;
        }
        var yn = 0, ut = null, _t = null, Qt = null, zs = !1, li = !1, wa = !1, Ls = 0, ll = 0, si = null, tS = 0;
        function Gt() {
            throw Error(r(321));
        }
        function su(t, e) {
            if (e === null) return !1;
            for(var i = 0; i < e.length && i < t.length; i++)if (!_e(t[i], e[i])) return !1;
            return !0;
        }
        function ru(t, e, i, s, o, c) {
            return yn = c, ut = e, e.memoizedState = null, e.updateQueue = null, e.lanes = 0, G.H = t === null || t.memoizedState === null ? wm : Tu, wa = !1, c = i(s, o), wa = !1, li && (c = Kh(e, i, s, o)), Xh(t), c;
        }
        function Xh(t) {
            G.H = ol;
            var e = _t !== null && _t.next !== null;
            if (yn = 0, Qt = _t = ut = null, zs = !1, ll = 0, si = null, e) throw Error(r(300));
            t === null || Ft || (t = t.dependencies, t !== null && Cs(t) && (Ft = !0));
        }
        function Kh(t, e, i, s) {
            ut = t;
            var o = 0;
            do {
                if (li && (si = null), ll = 0, li = !1, 25 <= o) throw Error(r(301));
                if (o += 1, Qt = _t = null, t.updateQueue != null) {
                    var c = t.updateQueue;
                    c.lastEffect = null, c.events = null, c.stores = null, c.memoCache != null && (c.memoCache.index = 0);
                }
                G.H = _m, c = e(i, s);
            }while (li);
            return c;
        }
        function eS() {
            var t = G.H, e = t.useState()[0];
            return e = typeof e.then == "function" ? sl(e) : e, t = t.useState()[0], (_t !== null ? _t.memoizedState : null) !== t && (ut.flags |= 1024), e;
        }
        function ou() {
            var t = Ls !== 0;
            return Ls = 0, t;
        }
        function uu(t, e, i) {
            e.updateQueue = t.updateQueue, e.flags &= -2053, t.lanes &= ~i;
        }
        function cu(t) {
            if (zs) {
                for(t = t.memoizedState; t !== null;){
                    var e = t.queue;
                    e !== null && (e.pending = null), t = t.next;
                }
                zs = !1;
            }
            yn = 0, Qt = _t = ut = null, li = !1, ll = Ls = 0, si = null;
        }
        function de() {
            var t = {
                memoizedState: null,
                baseState: null,
                baseQueue: null,
                queue: null,
                next: null
            };
            return Qt === null ? ut.memoizedState = Qt = t : Qt = Qt.next = t, Qt;
        }
        function Pt() {
            if (_t === null) {
                var t = ut.alternate;
                t = t !== null ? t.memoizedState : null;
            } else t = _t.next;
            var e = Qt === null ? ut.memoizedState : Qt.next;
            if (e !== null) Qt = e, _t = t;
            else {
                if (t === null) throw ut.alternate === null ? Error(r(467)) : Error(r(310));
                _t = t, t = {
                    memoizedState: _t.memoizedState,
                    baseState: _t.baseState,
                    baseQueue: _t.baseQueue,
                    queue: _t.queue,
                    next: null
                }, Qt === null ? ut.memoizedState = Qt = t : Qt = Qt.next = t;
            }
            return Qt;
        }
        function Vs() {
            return {
                lastEffect: null,
                events: null,
                stores: null,
                memoCache: null
            };
        }
        function sl(t) {
            var e = ll;
            return ll += 1, si === null && (si = []), t = Lh(si, t, e), e = ut, (Qt === null ? e.memoizedState : Qt.next) === null && (e = e.alternate, G.H = e === null || e.memoizedState === null ? wm : Tu), t;
        }
        function Bs(t) {
            if (t !== null && typeof t == "object") {
                if (typeof t.then == "function") return sl(t);
                if (t.$$typeof === V) return re(t);
            }
            throw Error(r(438, String(t)));
        }
        function fu(t) {
            var e = null, i = ut.updateQueue;
            if (i !== null && (e = i.memoCache), e == null) {
                var s = ut.alternate;
                s !== null && (s = s.updateQueue, s !== null && (s = s.memoCache, s != null && (e = {
                    data: s.data.map(function(o) {
                        return o.slice();
                    }),
                    index: 0
                })));
            }
            if (e == null && (e = {
                data: [],
                index: 0
            }), i === null && (i = Vs(), ut.updateQueue = i), i.memoCache = e, i = e.data[e.index], i === void 0) for(i = e.data[e.index] = Array(t), s = 0; s < t; s++)i[s] = it;
            return e.index++, i;
        }
        function vn(t, e) {
            return typeof e == "function" ? e(t) : e;
        }
        function Us(t) {
            var e = Pt();
            return du(e, _t, t);
        }
        function du(t, e, i) {
            var s = t.queue;
            if (s === null) throw Error(r(311));
            s.lastRenderedReducer = i;
            var o = t.baseQueue, c = s.pending;
            if (c !== null) {
                if (o !== null) {
                    var g = o.next;
                    o.next = c.next, c.next = g;
                }
                e.baseQueue = o = c, s.pending = null;
            }
            if (c = t.baseState, o === null) t.memoizedState = c;
            else {
                e = o.next;
                var b = g = null, C = null, O = e, q = !1;
                do {
                    var K = O.lane & -536870913;
                    if (K !== O.lane ? (mt & K) === K : (yn & K) === K) {
                        var N = O.revertLane;
                        if (N === 0) C !== null && (C = C.next = {
                            lane: 0,
                            revertLane: 0,
                            gesture: null,
                            action: O.action,
                            hasEagerState: O.hasEagerState,
                            eagerState: O.eagerState,
                            next: null
                        }), K === ti && (q = !0);
                        else if ((yn & N) === N) {
                            O = O.next, N === ti && (q = !0);
                            continue;
                        } else K = {
                            lane: 0,
                            revertLane: O.revertLane,
                            gesture: null,
                            action: O.action,
                            hasEagerState: O.hasEagerState,
                            eagerState: O.eagerState,
                            next: null
                        }, C === null ? (b = C = K, g = c) : C = C.next = K, ut.lanes |= N, Qn |= N;
                        K = O.action, wa && i(c, K), c = O.hasEagerState ? O.eagerState : i(c, K);
                    } else N = {
                        lane: K,
                        revertLane: O.revertLane,
                        gesture: O.gesture,
                        action: O.action,
                        hasEagerState: O.hasEagerState,
                        eagerState: O.eagerState,
                        next: null
                    }, C === null ? (b = C = N, g = c) : C = C.next = N, ut.lanes |= K, Qn |= K;
                    O = O.next;
                }while (O !== null && O !== e);
                if (C === null ? g = c : C.next = b, !_e(c, t.memoizedState) && (Ft = !0, q && (i = ei, i !== null))) throw i;
                t.memoizedState = c, t.baseState = g, t.baseQueue = C, s.lastRenderedState = c;
            }
            return o === null && (s.lanes = 0), [
                t.memoizedState,
                s.dispatch
            ];
        }
        function hu(t) {
            var e = Pt(), i = e.queue;
            if (i === null) throw Error(r(311));
            i.lastRenderedReducer = t;
            var s = i.dispatch, o = i.pending, c = e.memoizedState;
            if (o !== null) {
                i.pending = null;
                var g = o = o.next;
                do c = t(c, g.action), g = g.next;
                while (g !== o);
                _e(c, e.memoizedState) || (Ft = !0), e.memoizedState = c, e.baseQueue === null && (e.baseState = c), i.lastRenderedState = c;
            }
            return [
                c,
                s
            ];
        }
        function Ph(t, e, i) {
            var s = ut, o = Pt(), c = vt;
            if (c) {
                if (i === void 0) throw Error(r(407));
                i = i();
            } else i = e();
            var g = !_e((_t || o).memoizedState, i);
            if (g && (o.memoizedState = i, Ft = !0), o = o.queue, gu(Fh.bind(null, s, o, t), [
                t
            ]), o.getSnapshot !== e || g || Qt !== null && Qt.memoizedState.tag & 1) {
                if (s.flags |= 2048, ri(9, {
                    destroy: void 0
                }, Qh.bind(null, s, o, i, e), null), jt === null) throw Error(r(349));
                c || (yn & 127) !== 0 || Zh(s, e, i);
            }
            return i;
        }
        function Zh(t, e, i) {
            t.flags |= 16384, t = {
                getSnapshot: e,
                value: i
            }, e = ut.updateQueue, e === null ? (e = Vs(), ut.updateQueue = e, e.stores = [
                t
            ]) : (i = e.stores, i === null ? e.stores = [
                t
            ] : i.push(t));
        }
        function Qh(t, e, i, s) {
            e.value = i, e.getSnapshot = s, $h(e) && Jh(t);
        }
        function Fh(t, e, i) {
            return i(function() {
                $h(e) && Jh(t);
            });
        }
        function $h(t) {
            var e = t.getSnapshot;
            t = t.value;
            try {
                var i = e();
                return !_e(t, i);
            } catch  {
                return !0;
            }
        }
        function Jh(t) {
            var e = ya(t, 2);
            e !== null && Ee(e, t, 2);
        }
        function mu(t) {
            var e = de();
            if (typeof t == "function") {
                var i = t;
                if (t = i(), wa) {
                    zn(!0);
                    try {
                        i();
                    } finally{
                        zn(!1);
                    }
                }
            }
            return e.memoizedState = e.baseState = t, e.queue = {
                pending: null,
                lanes: 0,
                dispatch: null,
                lastRenderedReducer: vn,
                lastRenderedState: t
            }, e;
        }
        function Wh(t, e, i, s) {
            return t.baseState = i, du(t, _t, typeof s == "function" ? s : vn);
        }
        function nS(t, e, i, s, o) {
            if (qs(t)) throw Error(r(485));
            if (t = e.action, t !== null) {
                var c = {
                    payload: o,
                    action: t,
                    next: null,
                    isTransition: !0,
                    status: "pending",
                    value: null,
                    reason: null,
                    listeners: [],
                    then: function(g) {
                        c.listeners.push(g);
                    }
                };
                G.T !== null ? i(!0) : c.isTransition = !1, s(c), i = e.pending, i === null ? (c.next = e.pending = c, Ih(e, c)) : (c.next = i.next, e.pending = i.next = c);
            }
        }
        function Ih(t, e) {
            var i = e.action, s = e.payload, o = t.state;
            if (e.isTransition) {
                var c = G.T, g = {};
                G.T = g;
                try {
                    var b = i(o, s), C = G.S;
                    C !== null && C(g, b), tm(t, e, b);
                } catch (O) {
                    pu(t, e, O);
                } finally{
                    c !== null && g.types !== null && (c.types = g.types), G.T = c;
                }
            } else try {
                c = i(o, s), tm(t, e, c);
            } catch (O) {
                pu(t, e, O);
            }
        }
        function tm(t, e, i) {
            i !== null && typeof i == "object" && typeof i.then == "function" ? i.then(function(s) {
                em(t, e, s);
            }, function(s) {
                return pu(t, e, s);
            }) : em(t, e, i);
        }
        function em(t, e, i) {
            e.status = "fulfilled", e.value = i, nm(e), t.state = i, e = t.pending, e !== null && (i = e.next, i === e ? t.pending = null : (i = i.next, e.next = i, Ih(t, i)));
        }
        function pu(t, e, i) {
            var s = t.pending;
            if (t.pending = null, s !== null) {
                s = s.next;
                do e.status = "rejected", e.reason = i, nm(e), e = e.next;
                while (e !== s);
            }
            t.action = null;
        }
        function nm(t) {
            t = t.listeners;
            for(var e = 0; e < t.length; e++)(0, t[e])();
        }
        function am(t, e) {
            return e;
        }
        function im(t, e) {
            if (vt) {
                var i = jt.formState;
                if (i !== null) {
                    t: {
                        var s = ut;
                        if (vt) {
                            if (zt) {
                                e: {
                                    for(var o = zt, c = Ge; o.nodeType !== 8;){
                                        if (!c) {
                                            o = null;
                                            break e;
                                        }
                                        if (o = ke(o.nextSibling), o === null) {
                                            o = null;
                                            break e;
                                        }
                                    }
                                    c = o.data, o = c === "F!" || c === "F" ? o : null;
                                }
                                if (o) {
                                    zt = ke(o.nextSibling), s = o.data === "F!";
                                    break t;
                                }
                            }
                            Hn(s);
                        }
                        s = !1;
                    }
                    s && (e = i[0]);
                }
            }
            return i = de(), i.memoizedState = i.baseState = e, s = {
                pending: null,
                lanes: 0,
                dispatch: null,
                lastRenderedReducer: am,
                lastRenderedState: e
            }, i.queue = s, i = Em.bind(null, ut, s), s.dispatch = i, s = mu(!1), c = Su.bind(null, ut, !1, s.queue), s = de(), o = {
                state: e,
                dispatch: null,
                action: t,
                pending: null
            }, s.queue = o, i = nS.bind(null, ut, o, c, i), o.dispatch = i, s.memoizedState = t, [
                e,
                i,
                !1
            ];
        }
        function lm(t) {
            var e = Pt();
            return sm(e, _t, t);
        }
        function sm(t, e, i) {
            if (e = du(t, e, am)[0], t = Us(vn)[0], typeof e == "object" && e !== null && typeof e.then == "function") try {
                var s = sl(e);
            } catch (g) {
                throw g === ni ? Rs : g;
            }
            else s = e;
            e = Pt();
            var o = e.queue, c = o.dispatch;
            return i !== e.memoizedState && (ut.flags |= 2048, ri(9, {
                destroy: void 0
            }, aS.bind(null, o, i), null)), [
                s,
                c,
                t
            ];
        }
        function aS(t, e) {
            t.action = e;
        }
        function rm(t) {
            var e = Pt(), i = _t;
            if (i !== null) return sm(e, i, t);
            Pt(), e = e.memoizedState, i = Pt();
            var s = i.queue.dispatch;
            return i.memoizedState = t, [
                e,
                s,
                !1
            ];
        }
        function ri(t, e, i, s) {
            return t = {
                tag: t,
                create: i,
                deps: s,
                inst: e,
                next: null
            }, e = ut.updateQueue, e === null && (e = Vs(), ut.updateQueue = e), i = e.lastEffect, i === null ? e.lastEffect = t.next = t : (s = i.next, i.next = t, t.next = s, e.lastEffect = t), t;
        }
        function om() {
            return Pt().memoizedState;
        }
        function Hs(t, e, i, s) {
            var o = de();
            ut.flags |= t, o.memoizedState = ri(1 | e, {
                destroy: void 0
            }, i, s === void 0 ? null : s);
        }
        function Gs(t, e, i, s) {
            var o = Pt();
            s = s === void 0 ? null : s;
            var c = o.memoizedState.inst;
            _t !== null && s !== null && su(s, _t.memoizedState.deps) ? o.memoizedState = ri(e, c, i, s) : (ut.flags |= t, o.memoizedState = ri(1 | e, c, i, s));
        }
        function um(t, e) {
            Hs(8390656, 8, t, e);
        }
        function gu(t, e) {
            Gs(2048, 8, t, e);
        }
        function iS(t) {
            ut.flags |= 4;
            var e = ut.updateQueue;
            if (e === null) e = Vs(), ut.updateQueue = e, e.events = [
                t
            ];
            else {
                var i = e.events;
                i === null ? e.events = [
                    t
                ] : i.push(t);
            }
        }
        function cm(t) {
            var e = Pt().memoizedState;
            return iS({
                ref: e,
                nextImpl: t
            }), function() {
                if ((Et & 2) !== 0) throw Error(r(440));
                return e.impl.apply(void 0, arguments);
            };
        }
        function fm(t, e) {
            return Gs(4, 2, t, e);
        }
        function dm(t, e) {
            return Gs(4, 4, t, e);
        }
        function hm(t, e) {
            if (typeof e == "function") {
                t = t();
                var i = e(t);
                return function() {
                    typeof i == "function" ? i() : e(null);
                };
            }
            if (e != null) return t = t(), e.current = t, function() {
                e.current = null;
            };
        }
        function mm(t, e, i) {
            i = i != null ? i.concat([
                t
            ]) : null, Gs(4, 4, hm.bind(null, e, t), i);
        }
        function yu() {}
        function pm(t, e) {
            var i = Pt();
            e = e === void 0 ? null : e;
            var s = i.memoizedState;
            return e !== null && su(e, s[1]) ? s[0] : (i.memoizedState = [
                t,
                e
            ], t);
        }
        function gm(t, e) {
            var i = Pt();
            e = e === void 0 ? null : e;
            var s = i.memoizedState;
            if (e !== null && su(e, s[1])) return s[0];
            if (s = t(), wa) {
                zn(!0);
                try {
                    t();
                } finally{
                    zn(!1);
                }
            }
            return i.memoizedState = [
                s,
                e
            ], s;
        }
        function vu(t, e, i) {
            return i === void 0 || (yn & 1073741824) !== 0 && (mt & 261930) === 0 ? t.memoizedState = e : (t.memoizedState = i, t = yp(), ut.lanes |= t, Qn |= t, i);
        }
        function ym(t, e, i, s) {
            return _e(i, e) ? i : ii.current !== null ? (t = vu(t, i, s), _e(t, e) || (Ft = !0), t) : (yn & 42) === 0 || (yn & 1073741824) !== 0 && (mt & 261930) === 0 ? (Ft = !0, t.memoizedState = i) : (t = yp(), ut.lanes |= t, Qn |= t, e);
        }
        function vm(t, e, i, s, o) {
            var c = F.p;
            F.p = c !== 0 && 8 > c ? c : 8;
            var g = G.T, b = {};
            G.T = b, Su(t, !1, e, i);
            try {
                var C = o(), O = G.S;
                if (O !== null && O(b, C), C !== null && typeof C == "object" && typeof C.then == "function") {
                    var q = Ix(C, s);
                    rl(t, e, q, Ne(t));
                } else rl(t, e, s, Ne(t));
            } catch (K) {
                rl(t, e, {
                    then: function() {},
                    status: "rejected",
                    reason: K
                }, Ne());
            } finally{
                F.p = c, g !== null && b.types !== null && (g.types = b.types), G.T = g;
            }
        }
        function lS() {}
        function bu(t, e, i, s) {
            if (t.tag !== 5) throw Error(r(476));
            var o = bm(t).queue;
            vm(t, o, e, $, i === null ? lS : function() {
                return xm(t), i(s);
            });
        }
        function bm(t) {
            var e = t.memoizedState;
            if (e !== null) return e;
            e = {
                memoizedState: $,
                baseState: $,
                baseQueue: null,
                queue: {
                    pending: null,
                    lanes: 0,
                    dispatch: null,
                    lastRenderedReducer: vn,
                    lastRenderedState: $
                },
                next: null
            };
            var i = {};
            return e.next = {
                memoizedState: i,
                baseState: i,
                baseQueue: null,
                queue: {
                    pending: null,
                    lanes: 0,
                    dispatch: null,
                    lastRenderedReducer: vn,
                    lastRenderedState: i
                },
                next: null
            }, t.memoizedState = e, t = t.alternate, t !== null && (t.memoizedState = e), e;
        }
        function xm(t) {
            var e = bm(t);
            e.next === null && (e = t.alternate.memoizedState), rl(t, e.next.queue, {}, Ne());
        }
        function xu() {
            return re(Al);
        }
        function Sm() {
            return Pt().memoizedState;
        }
        function Tm() {
            return Pt().memoizedState;
        }
        function sS(t) {
            for(var e = t.return; e !== null;){
                switch(e.tag){
                    case 24:
                    case 3:
                        var i = Ne();
                        t = kn(i);
                        var s = Yn(e, t, i);
                        s !== null && (Ee(s, e, i), nl(s, e, i)), e = {
                            cache: Fo()
                        }, t.payload = e;
                        return;
                }
                e = e.return;
            }
        }
        function rS(t, e, i) {
            var s = Ne();
            i = {
                lane: s,
                revertLane: 0,
                gesture: null,
                action: i,
                hasEagerState: !1,
                eagerState: null,
                next: null
            }, qs(t) ? Am(e, i) : (i = Uo(t, e, i, s), i !== null && (Ee(i, t, s), Cm(i, e, s)));
        }
        function Em(t, e, i) {
            var s = Ne();
            rl(t, e, i, s);
        }
        function rl(t, e, i, s) {
            var o = {
                lane: s,
                revertLane: 0,
                gesture: null,
                action: i,
                hasEagerState: !1,
                eagerState: null,
                next: null
            };
            if (qs(t)) Am(e, o);
            else {
                var c = t.alternate;
                if (t.lanes === 0 && (c === null || c.lanes === 0) && (c = e.lastRenderedReducer, c !== null)) try {
                    var g = e.lastRenderedState, b = c(g, i);
                    if (o.hasEagerState = !0, o.eagerState = b, _e(b, g)) return Ss(t, e, o, 0), jt === null && xs(), !1;
                } catch  {} finally{}
                if (i = Uo(t, e, o, s), i !== null) return Ee(i, t, s), Cm(i, e, s), !0;
            }
            return !1;
        }
        function Su(t, e, i, s) {
            if (s = {
                lane: 2,
                revertLane: Iu(),
                gesture: null,
                action: s,
                hasEagerState: !1,
                eagerState: null,
                next: null
            }, qs(t)) {
                if (e) throw Error(r(479));
            } else e = Uo(t, i, s, 2), e !== null && Ee(e, t, 2);
        }
        function qs(t) {
            var e = t.alternate;
            return t === ut || e !== null && e === ut;
        }
        function Am(t, e) {
            li = zs = !0;
            var i = t.pending;
            i === null ? e.next = e : (e.next = i.next, i.next = e), t.pending = e;
        }
        function Cm(t, e, i) {
            if ((i & 4194048) !== 0) {
                var s = e.lanes;
                s &= t.pendingLanes, i |= s, e.lanes = i, Dd(t, i);
            }
        }
        var ol = {
            readContext: re,
            use: Bs,
            useCallback: Gt,
            useContext: Gt,
            useEffect: Gt,
            useImperativeHandle: Gt,
            useLayoutEffect: Gt,
            useInsertionEffect: Gt,
            useMemo: Gt,
            useReducer: Gt,
            useRef: Gt,
            useState: Gt,
            useDebugValue: Gt,
            useDeferredValue: Gt,
            useTransition: Gt,
            useSyncExternalStore: Gt,
            useId: Gt,
            useHostTransitionStatus: Gt,
            useFormState: Gt,
            useActionState: Gt,
            useOptimistic: Gt,
            useMemoCache: Gt,
            useCacheRefresh: Gt
        };
        ol.useEffectEvent = Gt;
        var wm = {
            readContext: re,
            use: Bs,
            useCallback: function(t, e) {
                return de().memoizedState = [
                    t,
                    e === void 0 ? null : e
                ], t;
            },
            useContext: re,
            useEffect: um,
            useImperativeHandle: function(t, e, i) {
                i = i != null ? i.concat([
                    t
                ]) : null, Hs(4194308, 4, hm.bind(null, e, t), i);
            },
            useLayoutEffect: function(t, e) {
                return Hs(4194308, 4, t, e);
            },
            useInsertionEffect: function(t, e) {
                Hs(4, 2, t, e);
            },
            useMemo: function(t, e) {
                var i = de();
                e = e === void 0 ? null : e;
                var s = t();
                if (wa) {
                    zn(!0);
                    try {
                        t();
                    } finally{
                        zn(!1);
                    }
                }
                return i.memoizedState = [
                    s,
                    e
                ], s;
            },
            useReducer: function(t, e, i) {
                var s = de();
                if (i !== void 0) {
                    var o = i(e);
                    if (wa) {
                        zn(!0);
                        try {
                            i(e);
                        } finally{
                            zn(!1);
                        }
                    }
                } else o = e;
                return s.memoizedState = s.baseState = o, t = {
                    pending: null,
                    lanes: 0,
                    dispatch: null,
                    lastRenderedReducer: t,
                    lastRenderedState: o
                }, s.queue = t, t = t.dispatch = rS.bind(null, ut, t), [
                    s.memoizedState,
                    t
                ];
            },
            useRef: function(t) {
                var e = de();
                return t = {
                    current: t
                }, e.memoizedState = t;
            },
            useState: function(t) {
                t = mu(t);
                var e = t.queue, i = Em.bind(null, ut, e);
                return e.dispatch = i, [
                    t.memoizedState,
                    i
                ];
            },
            useDebugValue: yu,
            useDeferredValue: function(t, e) {
                var i = de();
                return vu(i, t, e);
            },
            useTransition: function() {
                var t = mu(!1);
                return t = vm.bind(null, ut, t.queue, !0, !1), de().memoizedState = t, [
                    !1,
                    t
                ];
            },
            useSyncExternalStore: function(t, e, i) {
                var s = ut, o = de();
                if (vt) {
                    if (i === void 0) throw Error(r(407));
                    i = i();
                } else {
                    if (i = e(), jt === null) throw Error(r(349));
                    (mt & 127) !== 0 || Zh(s, e, i);
                }
                o.memoizedState = i;
                var c = {
                    value: i,
                    getSnapshot: e
                };
                return o.queue = c, um(Fh.bind(null, s, c, t), [
                    t
                ]), s.flags |= 2048, ri(9, {
                    destroy: void 0
                }, Qh.bind(null, s, c, i, e), null), i;
            },
            useId: function() {
                var t = de(), e = jt.identifierPrefix;
                if (vt) {
                    var i = en, s = tn;
                    i = (s & ~(1 << 32 - we(s) - 1)).toString(32) + i, e = "_" + e + "R_" + i, i = Ls++, 0 < i && (e += "H" + i.toString(32)), e += "_";
                } else i = tS++, e = "_" + e + "r_" + i.toString(32) + "_";
                return t.memoizedState = e;
            },
            useHostTransitionStatus: xu,
            useFormState: im,
            useActionState: im,
            useOptimistic: function(t) {
                var e = de();
                e.memoizedState = e.baseState = t;
                var i = {
                    pending: null,
                    lanes: 0,
                    dispatch: null,
                    lastRenderedReducer: null,
                    lastRenderedState: null
                };
                return e.queue = i, e = Su.bind(null, ut, !0, i), i.dispatch = e, [
                    t,
                    e
                ];
            },
            useMemoCache: fu,
            useCacheRefresh: function() {
                return de().memoizedState = sS.bind(null, ut);
            },
            useEffectEvent: function(t) {
                var e = de(), i = {
                    impl: t
                };
                return e.memoizedState = i, function() {
                    if ((Et & 2) !== 0) throw Error(r(440));
                    return i.impl.apply(void 0, arguments);
                };
            }
        }, Tu = {
            readContext: re,
            use: Bs,
            useCallback: pm,
            useContext: re,
            useEffect: gu,
            useImperativeHandle: mm,
            useInsertionEffect: fm,
            useLayoutEffect: dm,
            useMemo: gm,
            useReducer: Us,
            useRef: om,
            useState: function() {
                return Us(vn);
            },
            useDebugValue: yu,
            useDeferredValue: function(t, e) {
                var i = Pt();
                return ym(i, _t.memoizedState, t, e);
            },
            useTransition: function() {
                var t = Us(vn)[0], e = Pt().memoizedState;
                return [
                    typeof t == "boolean" ? t : sl(t),
                    e
                ];
            },
            useSyncExternalStore: Ph,
            useId: Sm,
            useHostTransitionStatus: xu,
            useFormState: lm,
            useActionState: lm,
            useOptimistic: function(t, e) {
                var i = Pt();
                return Wh(i, _t, t, e);
            },
            useMemoCache: fu,
            useCacheRefresh: Tm
        };
        Tu.useEffectEvent = cm;
        var _m = {
            readContext: re,
            use: Bs,
            useCallback: pm,
            useContext: re,
            useEffect: gu,
            useImperativeHandle: mm,
            useInsertionEffect: fm,
            useLayoutEffect: dm,
            useMemo: gm,
            useReducer: hu,
            useRef: om,
            useState: function() {
                return hu(vn);
            },
            useDebugValue: yu,
            useDeferredValue: function(t, e) {
                var i = Pt();
                return _t === null ? vu(i, t, e) : ym(i, _t.memoizedState, t, e);
            },
            useTransition: function() {
                var t = hu(vn)[0], e = Pt().memoizedState;
                return [
                    typeof t == "boolean" ? t : sl(t),
                    e
                ];
            },
            useSyncExternalStore: Ph,
            useId: Sm,
            useHostTransitionStatus: xu,
            useFormState: rm,
            useActionState: rm,
            useOptimistic: function(t, e) {
                var i = Pt();
                return _t !== null ? Wh(i, _t, t, e) : (i.baseState = t, [
                    t,
                    i.queue.dispatch
                ]);
            },
            useMemoCache: fu,
            useCacheRefresh: Tm
        };
        _m.useEffectEvent = cm;
        function Eu(t, e, i, s) {
            e = t.memoizedState, i = i(s, e), i = i == null ? e : v({}, e, i), t.memoizedState = i, t.lanes === 0 && (t.updateQueue.baseState = i);
        }
        var Au = {
            enqueueSetState: function(t, e, i) {
                t = t._reactInternals;
                var s = Ne(), o = kn(s);
                o.payload = e, i != null && (o.callback = i), e = Yn(t, o, s), e !== null && (Ee(e, t, s), nl(e, t, s));
            },
            enqueueReplaceState: function(t, e, i) {
                t = t._reactInternals;
                var s = Ne(), o = kn(s);
                o.tag = 1, o.payload = e, i != null && (o.callback = i), e = Yn(t, o, s), e !== null && (Ee(e, t, s), nl(e, t, s));
            },
            enqueueForceUpdate: function(t, e) {
                t = t._reactInternals;
                var i = Ne(), s = kn(i);
                s.tag = 2, e != null && (s.callback = e), e = Yn(t, s, i), e !== null && (Ee(e, t, i), nl(e, t, i));
            }
        };
        function Rm(t, e, i, s, o, c, g) {
            return t = t.stateNode, typeof t.shouldComponentUpdate == "function" ? t.shouldComponentUpdate(s, c, g) : e.prototype && e.prototype.isPureReactComponent ? !Qi(i, s) || !Qi(o, c) : !0;
        }
        function Mm(t, e, i, s) {
            t = e.state, typeof e.componentWillReceiveProps == "function" && e.componentWillReceiveProps(i, s), typeof e.UNSAFE_componentWillReceiveProps == "function" && e.UNSAFE_componentWillReceiveProps(i, s), e.state !== t && Au.enqueueReplaceState(e, e.state, null);
        }
        function _a(t, e) {
            var i = e;
            if ("ref" in e) {
                i = {};
                for(var s in e)s !== "ref" && (i[s] = e[s]);
            }
            if (t = t.defaultProps) {
                i === e && (i = v({}, i));
                for(var o in t)i[o] === void 0 && (i[o] = t[o]);
            }
            return i;
        }
        function Dm(t) {
            bs(t);
        }
        function jm(t) {
            console.error(t);
        }
        function Om(t) {
            bs(t);
        }
        function ks(t, e) {
            try {
                var i = t.onUncaughtError;
                i(e.value, {
                    componentStack: e.stack
                });
            } catch (s) {
                setTimeout(function() {
                    throw s;
                });
            }
        }
        function Nm(t, e, i) {
            try {
                var s = t.onCaughtError;
                s(i.value, {
                    componentStack: i.stack,
                    errorBoundary: e.tag === 1 ? e.stateNode : null
                });
            } catch (o) {
                setTimeout(function() {
                    throw o;
                });
            }
        }
        function Cu(t, e, i) {
            return i = kn(i), i.tag = 3, i.payload = {
                element: null
            }, i.callback = function() {
                ks(t, e);
            }, i;
        }
        function zm(t) {
            return t = kn(t), t.tag = 3, t;
        }
        function Lm(t, e, i, s) {
            var o = i.type.getDerivedStateFromError;
            if (typeof o == "function") {
                var c = s.value;
                t.payload = function() {
                    return o(c);
                }, t.callback = function() {
                    Nm(e, i, s);
                };
            }
            var g = i.stateNode;
            g !== null && typeof g.componentDidCatch == "function" && (t.callback = function() {
                Nm(e, i, s), typeof o != "function" && (Fn === null ? Fn = new Set([
                    this
                ]) : Fn.add(this));
                var b = s.stack;
                this.componentDidCatch(s.value, {
                    componentStack: b !== null ? b : ""
                });
            });
        }
        function oS(t, e, i, s, o) {
            if (i.flags |= 32768, s !== null && typeof s == "object" && typeof s.then == "function") {
                if (e = i.alternate, e !== null && Ia(e, i, o, !0), i = Me.current, i !== null) {
                    switch(i.tag){
                        case 31:
                        case 13:
                            return qe === null ? tr() : i.alternate === null && qt === 0 && (qt = 3), i.flags &= -257, i.flags |= 65536, i.lanes = o, s === Ms ? i.flags |= 16384 : (e = i.updateQueue, e === null ? i.updateQueue = new Set([
                                s
                            ]) : e.add(s), $u(t, s, o)), !1;
                        case 22:
                            return i.flags |= 65536, s === Ms ? i.flags |= 16384 : (e = i.updateQueue, e === null ? (e = {
                                transitions: null,
                                markerInstances: null,
                                retryQueue: new Set([
                                    s
                                ])
                            }, i.updateQueue = e) : (i = e.retryQueue, i === null ? e.retryQueue = new Set([
                                s
                            ]) : i.add(s)), $u(t, s, o)), !1;
                    }
                    throw Error(r(435, i.tag));
                }
                return $u(t, s, o), tr(), !1;
            }
            if (vt) return e = Me.current, e !== null ? ((e.flags & 65536) === 0 && (e.flags |= 256), e.flags |= 65536, e.lanes = o, s !== Xo && (t = Error(r(422), {
                cause: s
            }), Ji(Be(t, i)))) : (s !== Xo && (e = Error(r(423), {
                cause: s
            }), Ji(Be(e, i))), t = t.current.alternate, t.flags |= 65536, o &= -o, t.lanes |= o, s = Be(s, i), o = Cu(t.stateNode, s, o), eu(t, o), qt !== 4 && (qt = 2)), !1;
            var c = Error(r(520), {
                cause: s
            });
            if (c = Be(c, i), gl === null ? gl = [
                c
            ] : gl.push(c), qt !== 4 && (qt = 2), e === null) return !0;
            s = Be(s, i), i = e;
            do {
                switch(i.tag){
                    case 3:
                        return i.flags |= 65536, t = o & -o, i.lanes |= t, t = Cu(i.stateNode, s, t), eu(i, t), !1;
                    case 1:
                        if (e = i.type, c = i.stateNode, (i.flags & 128) === 0 && (typeof e.getDerivedStateFromError == "function" || c !== null && typeof c.componentDidCatch == "function" && (Fn === null || !Fn.has(c)))) return i.flags |= 65536, o &= -o, i.lanes |= o, o = zm(o), Lm(o, t, i, s), eu(i, o), !1;
                }
                i = i.return;
            }while (i !== null);
            return !1;
        }
        var wu = Error(r(461)), Ft = !1;
        function oe(t, e, i, s) {
            e.child = t === null ? Hh(e, null, i, s) : Ca(e, t.child, i, s);
        }
        function Vm(t, e, i, s, o) {
            i = i.render;
            var c = e.ref;
            if ("ref" in s) {
                var g = {};
                for(var b in s)b !== "ref" && (g[b] = s[b]);
            } else g = s;
            return Sa(e), s = ru(t, e, i, g, c, o), b = ou(), t !== null && !Ft ? (uu(t, e, o), bn(t, e, o)) : (vt && b && ko(e), e.flags |= 1, oe(t, e, s, o), e.child);
        }
        function Bm(t, e, i, s, o) {
            if (t === null) {
                var c = i.type;
                return typeof c == "function" && !Ho(c) && c.defaultProps === void 0 && i.compare === null ? (e.tag = 15, e.type = c, Um(t, e, c, s, o)) : (t = Es(i.type, null, s, e, e.mode, o), t.ref = e.ref, t.return = e, e.child = t);
            }
            if (c = t.child, !zu(t, o)) {
                var g = c.memoizedProps;
                if (i = i.compare, i = i !== null ? i : Qi, i(g, s) && t.ref === e.ref) return bn(t, e, o);
            }
            return e.flags |= 1, t = hn(c, s), t.ref = e.ref, t.return = e, e.child = t;
        }
        function Um(t, e, i, s, o) {
            if (t !== null) {
                var c = t.memoizedProps;
                if (Qi(c, s) && t.ref === e.ref) if (Ft = !1, e.pendingProps = s = c, zu(t, o)) (t.flags & 131072) !== 0 && (Ft = !0);
                else return e.lanes = t.lanes, bn(t, e, o);
            }
            return _u(t, e, i, s, o);
        }
        function Hm(t, e, i, s) {
            var o = s.children, c = t !== null ? t.memoizedState : null;
            if (t === null && e.stateNode === null && (e.stateNode = {
                _visibility: 1,
                _pendingMarkers: null,
                _retryCache: null,
                _transitions: null
            }), s.mode === "hidden") {
                if ((e.flags & 128) !== 0) {
                    if (c = c !== null ? c.baseLanes | i : i, t !== null) {
                        for(s = e.child = t.child, o = 0; s !== null;)o = o | s.lanes | s.childLanes, s = s.sibling;
                        s = o & ~c;
                    } else s = 0, e.child = null;
                    return Gm(t, e, c, i, s);
                }
                if ((i & 536870912) !== 0) e.memoizedState = {
                    baseLanes: 0,
                    cachePool: null
                }, t !== null && _s(e, c !== null ? c.cachePool : null), c !== null ? kh(e, c) : au(), Yh(e);
                else return s = e.lanes = 536870912, Gm(t, e, c !== null ? c.baseLanes | i : i, i, s);
            } else c !== null ? (_s(e, c.cachePool), kh(e, c), Kn(), e.memoizedState = null) : (t !== null && _s(e, null), au(), Kn());
            return oe(t, e, o, i), e.child;
        }
        function ul(t, e) {
            return t !== null && t.tag === 22 || e.stateNode !== null || (e.stateNode = {
                _visibility: 1,
                _pendingMarkers: null,
                _retryCache: null,
                _transitions: null
            }), e.sibling;
        }
        function Gm(t, e, i, s, o) {
            var c = Jo();
            return c = c === null ? null : {
                parent: Zt._currentValue,
                pool: c
            }, e.memoizedState = {
                baseLanes: i,
                cachePool: c
            }, t !== null && _s(e, null), au(), Yh(e), t !== null && Ia(t, e, s, !0), e.childLanes = o, null;
        }
        function Ys(t, e) {
            return e = Ks({
                mode: e.mode,
                children: e.children
            }, t.mode), e.ref = t.ref, t.child = e, e.return = t, e;
        }
        function qm(t, e, i) {
            return Ca(e, t.child, null, i), t = Ys(e, e.pendingProps), t.flags |= 2, De(e), e.memoizedState = null, t;
        }
        function uS(t, e, i) {
            var s = e.pendingProps, o = (e.flags & 128) !== 0;
            if (e.flags &= -129, t === null) {
                if (vt) {
                    if (s.mode === "hidden") return t = Ys(e, s), e.lanes = 536870912, ul(null, t);
                    if (lu(e), (t = zt) ? (t = Ip(t, Ge), t = t !== null && t.data === "&" ? t : null, t !== null && (e.memoizedState = {
                        dehydrated: t,
                        treeContext: Bn !== null ? {
                            id: tn,
                            overflow: en
                        } : null,
                        retryLane: 536870912,
                        hydrationErrors: null
                    }, i = Ah(t), i.return = e, e.child = i, se = e, zt = null)) : t = null, t === null) throw Hn(e);
                    return e.lanes = 536870912, null;
                }
                return Ys(e, s);
            }
            var c = t.memoizedState;
            if (c !== null) {
                var g = c.dehydrated;
                if (lu(e), o) if (e.flags & 256) e.flags &= -257, e = qm(t, e, i);
                else if (e.memoizedState !== null) e.child = t.child, e.flags |= 128, e = null;
                else throw Error(r(558));
                else if (Ft || Ia(t, e, i, !1), o = (i & t.childLanes) !== 0, Ft || o) {
                    if (s = jt, s !== null && (g = jd(s, i), g !== 0 && g !== c.retryLane)) throw c.retryLane = g, ya(t, g), Ee(s, t, g), wu;
                    tr(), e = qm(t, e, i);
                } else t = c.treeContext, zt = ke(g.nextSibling), se = e, vt = !0, Un = null, Ge = !1, t !== null && _h(e, t), e = Ys(e, s), e.flags |= 4096;
                return e;
            }
            return t = hn(t.child, {
                mode: s.mode,
                children: s.children
            }), t.ref = e.ref, e.child = t, t.return = e, t;
        }
        function Xs(t, e) {
            var i = e.ref;
            if (i === null) t !== null && t.ref !== null && (e.flags |= 4194816);
            else {
                if (typeof i != "function" && typeof i != "object") throw Error(r(284));
                (t === null || t.ref !== i) && (e.flags |= 4194816);
            }
        }
        function _u(t, e, i, s, o) {
            return Sa(e), i = ru(t, e, i, s, void 0, o), s = ou(), t !== null && !Ft ? (uu(t, e, o), bn(t, e, o)) : (vt && s && ko(e), e.flags |= 1, oe(t, e, i, o), e.child);
        }
        function km(t, e, i, s, o, c) {
            return Sa(e), e.updateQueue = null, i = Kh(e, s, i, o), Xh(t), s = ou(), t !== null && !Ft ? (uu(t, e, c), bn(t, e, c)) : (vt && s && ko(e), e.flags |= 1, oe(t, e, i, c), e.child);
        }
        function Ym(t, e, i, s, o) {
            if (Sa(e), e.stateNode === null) {
                var c = Fa, g = i.contextType;
                typeof g == "object" && g !== null && (c = re(g)), c = new i(s, c), e.memoizedState = c.state !== null && c.state !== void 0 ? c.state : null, c.updater = Au, e.stateNode = c, c._reactInternals = e, c = e.stateNode, c.props = s, c.state = e.memoizedState, c.refs = {}, Io(e), g = i.contextType, c.context = typeof g == "object" && g !== null ? re(g) : Fa, c.state = e.memoizedState, g = i.getDerivedStateFromProps, typeof g == "function" && (Eu(e, i, g, s), c.state = e.memoizedState), typeof i.getDerivedStateFromProps == "function" || typeof c.getSnapshotBeforeUpdate == "function" || typeof c.UNSAFE_componentWillMount != "function" && typeof c.componentWillMount != "function" || (g = c.state, typeof c.componentWillMount == "function" && c.componentWillMount(), typeof c.UNSAFE_componentWillMount == "function" && c.UNSAFE_componentWillMount(), g !== c.state && Au.enqueueReplaceState(c, c.state, null), il(e, s, c, o), al(), c.state = e.memoizedState), typeof c.componentDidMount == "function" && (e.flags |= 4194308), s = !0;
            } else if (t === null) {
                c = e.stateNode;
                var b = e.memoizedProps, C = _a(i, b);
                c.props = C;
                var O = c.context, q = i.contextType;
                g = Fa, typeof q == "object" && q !== null && (g = re(q));
                var K = i.getDerivedStateFromProps;
                q = typeof K == "function" || typeof c.getSnapshotBeforeUpdate == "function", b = e.pendingProps !== b, q || typeof c.UNSAFE_componentWillReceiveProps != "function" && typeof c.componentWillReceiveProps != "function" || (b || O !== g) && Mm(e, c, s, g), qn = !1;
                var N = e.memoizedState;
                c.state = N, il(e, s, c, o), al(), O = e.memoizedState, b || N !== O || qn ? (typeof K == "function" && (Eu(e, i, K, s), O = e.memoizedState), (C = qn || Rm(e, i, C, s, N, O, g)) ? (q || typeof c.UNSAFE_componentWillMount != "function" && typeof c.componentWillMount != "function" || (typeof c.componentWillMount == "function" && c.componentWillMount(), typeof c.UNSAFE_componentWillMount == "function" && c.UNSAFE_componentWillMount()), typeof c.componentDidMount == "function" && (e.flags |= 4194308)) : (typeof c.componentDidMount == "function" && (e.flags |= 4194308), e.memoizedProps = s, e.memoizedState = O), c.props = s, c.state = O, c.context = g, s = C) : (typeof c.componentDidMount == "function" && (e.flags |= 4194308), s = !1);
            } else {
                c = e.stateNode, tu(t, e), g = e.memoizedProps, q = _a(i, g), c.props = q, K = e.pendingProps, N = c.context, O = i.contextType, C = Fa, typeof O == "object" && O !== null && (C = re(O)), b = i.getDerivedStateFromProps, (O = typeof b == "function" || typeof c.getSnapshotBeforeUpdate == "function") || typeof c.UNSAFE_componentWillReceiveProps != "function" && typeof c.componentWillReceiveProps != "function" || (g !== K || N !== C) && Mm(e, c, s, C), qn = !1, N = e.memoizedState, c.state = N, il(e, s, c, o), al();
                var L = e.memoizedState;
                g !== K || N !== L || qn || t !== null && t.dependencies !== null && Cs(t.dependencies) ? (typeof b == "function" && (Eu(e, i, b, s), L = e.memoizedState), (q = qn || Rm(e, i, q, s, N, L, C) || t !== null && t.dependencies !== null && Cs(t.dependencies)) ? (O || typeof c.UNSAFE_componentWillUpdate != "function" && typeof c.componentWillUpdate != "function" || (typeof c.componentWillUpdate == "function" && c.componentWillUpdate(s, L, C), typeof c.UNSAFE_componentWillUpdate == "function" && c.UNSAFE_componentWillUpdate(s, L, C)), typeof c.componentDidUpdate == "function" && (e.flags |= 4), typeof c.getSnapshotBeforeUpdate == "function" && (e.flags |= 1024)) : (typeof c.componentDidUpdate != "function" || g === t.memoizedProps && N === t.memoizedState || (e.flags |= 4), typeof c.getSnapshotBeforeUpdate != "function" || g === t.memoizedProps && N === t.memoizedState || (e.flags |= 1024), e.memoizedProps = s, e.memoizedState = L), c.props = s, c.state = L, c.context = C, s = q) : (typeof c.componentDidUpdate != "function" || g === t.memoizedProps && N === t.memoizedState || (e.flags |= 4), typeof c.getSnapshotBeforeUpdate != "function" || g === t.memoizedProps && N === t.memoizedState || (e.flags |= 1024), s = !1);
            }
            return c = s, Xs(t, e), s = (e.flags & 128) !== 0, c || s ? (c = e.stateNode, i = s && typeof i.getDerivedStateFromError != "function" ? null : c.render(), e.flags |= 1, t !== null && s ? (e.child = Ca(e, t.child, null, o), e.child = Ca(e, null, i, o)) : oe(t, e, i, o), e.memoizedState = c.state, t = e.child) : t = bn(t, e, o), t;
        }
        function Xm(t, e, i, s) {
            return ba(), e.flags |= 256, oe(t, e, i, s), e.child;
        }
        var Ru = {
            dehydrated: null,
            treeContext: null,
            retryLane: 0,
            hydrationErrors: null
        };
        function Mu(t) {
            return {
                baseLanes: t,
                cachePool: Nh()
            };
        }
        function Du(t, e, i) {
            return t = t !== null ? t.childLanes & ~i : 0, e && (t |= Oe), t;
        }
        function Km(t, e, i) {
            var s = e.pendingProps, o = !1, c = (e.flags & 128) !== 0, g;
            if ((g = c) || (g = t !== null && t.memoizedState === null ? !1 : (Kt.current & 2) !== 0), g && (o = !0, e.flags &= -129), g = (e.flags & 32) !== 0, e.flags &= -33, t === null) {
                if (vt) {
                    if (o ? Xn(e) : Kn(), (t = zt) ? (t = Ip(t, Ge), t = t !== null && t.data !== "&" ? t : null, t !== null && (e.memoizedState = {
                        dehydrated: t,
                        treeContext: Bn !== null ? {
                            id: tn,
                            overflow: en
                        } : null,
                        retryLane: 536870912,
                        hydrationErrors: null
                    }, i = Ah(t), i.return = e, e.child = i, se = e, zt = null)) : t = null, t === null) throw Hn(e);
                    return dc(t) ? e.lanes = 32 : e.lanes = 536870912, null;
                }
                var b = s.children;
                return s = s.fallback, o ? (Kn(), o = e.mode, b = Ks({
                    mode: "hidden",
                    children: b
                }, o), s = va(s, o, i, null), b.return = e, s.return = e, b.sibling = s, e.child = b, s = e.child, s.memoizedState = Mu(i), s.childLanes = Du(t, g, i), e.memoizedState = Ru, ul(null, s)) : (Xn(e), ju(e, b));
            }
            var C = t.memoizedState;
            if (C !== null && (b = C.dehydrated, b !== null)) {
                if (c) e.flags & 256 ? (Xn(e), e.flags &= -257, e = Ou(t, e, i)) : e.memoizedState !== null ? (Kn(), e.child = t.child, e.flags |= 128, e = null) : (Kn(), b = s.fallback, o = e.mode, s = Ks({
                    mode: "visible",
                    children: s.children
                }, o), b = va(b, o, i, null), b.flags |= 2, s.return = e, b.return = e, s.sibling = b, e.child = s, Ca(e, t.child, null, i), s = e.child, s.memoizedState = Mu(i), s.childLanes = Du(t, g, i), e.memoizedState = Ru, e = ul(null, s));
                else if (Xn(e), dc(b)) {
                    if (g = b.nextSibling && b.nextSibling.dataset, g) var O = g.dgst;
                    g = O, s = Error(r(419)), s.stack = "", s.digest = g, Ji({
                        value: s,
                        source: null,
                        stack: null
                    }), e = Ou(t, e, i);
                } else if (Ft || Ia(t, e, i, !1), g = (i & t.childLanes) !== 0, Ft || g) {
                    if (g = jt, g !== null && (s = jd(g, i), s !== 0 && s !== C.retryLane)) throw C.retryLane = s, ya(t, s), Ee(g, t, s), wu;
                    fc(b) || tr(), e = Ou(t, e, i);
                } else fc(b) ? (e.flags |= 192, e.child = t.child, e = null) : (t = C.treeContext, zt = ke(b.nextSibling), se = e, vt = !0, Un = null, Ge = !1, t !== null && _h(e, t), e = ju(e, s.children), e.flags |= 4096);
                return e;
            }
            return o ? (Kn(), b = s.fallback, o = e.mode, C = t.child, O = C.sibling, s = hn(C, {
                mode: "hidden",
                children: s.children
            }), s.subtreeFlags = C.subtreeFlags & 65011712, O !== null ? b = hn(O, b) : (b = va(b, o, i, null), b.flags |= 2), b.return = e, s.return = e, s.sibling = b, e.child = s, ul(null, s), s = e.child, b = t.child.memoizedState, b === null ? b = Mu(i) : (o = b.cachePool, o !== null ? (C = Zt._currentValue, o = o.parent !== C ? {
                parent: C,
                pool: C
            } : o) : o = Nh(), b = {
                baseLanes: b.baseLanes | i,
                cachePool: o
            }), s.memoizedState = b, s.childLanes = Du(t, g, i), e.memoizedState = Ru, ul(t.child, s)) : (Xn(e), i = t.child, t = i.sibling, i = hn(i, {
                mode: "visible",
                children: s.children
            }), i.return = e, i.sibling = null, t !== null && (g = e.deletions, g === null ? (e.deletions = [
                t
            ], e.flags |= 16) : g.push(t)), e.child = i, e.memoizedState = null, i);
        }
        function ju(t, e) {
            return e = Ks({
                mode: "visible",
                children: e
            }, t.mode), e.return = t, t.child = e;
        }
        function Ks(t, e) {
            return t = Re(22, t, null, e), t.lanes = 0, t;
        }
        function Ou(t, e, i) {
            return Ca(e, t.child, null, i), t = ju(e, e.pendingProps.children), t.flags |= 2, e.memoizedState = null, t;
        }
        function Pm(t, e, i) {
            t.lanes |= e;
            var s = t.alternate;
            s !== null && (s.lanes |= e), Zo(t.return, e, i);
        }
        function Nu(t, e, i, s, o, c) {
            var g = t.memoizedState;
            g === null ? t.memoizedState = {
                isBackwards: e,
                rendering: null,
                renderingStartTime: 0,
                last: s,
                tail: i,
                tailMode: o,
                treeForkCount: c
            } : (g.isBackwards = e, g.rendering = null, g.renderingStartTime = 0, g.last = s, g.tail = i, g.tailMode = o, g.treeForkCount = c);
        }
        function Zm(t, e, i) {
            var s = e.pendingProps, o = s.revealOrder, c = s.tail;
            s = s.children;
            var g = Kt.current, b = (g & 2) !== 0;
            if (b ? (g = g & 1 | 2, e.flags |= 128) : g &= 1, J(Kt, g), oe(t, e, s, i), s = vt ? $i : 0, !b && t !== null && (t.flags & 128) !== 0) t: for(t = e.child; t !== null;){
                if (t.tag === 13) t.memoizedState !== null && Pm(t, i, e);
                else if (t.tag === 19) Pm(t, i, e);
                else if (t.child !== null) {
                    t.child.return = t, t = t.child;
                    continue;
                }
                if (t === e) break t;
                for(; t.sibling === null;){
                    if (t.return === null || t.return === e) break t;
                    t = t.return;
                }
                t.sibling.return = t.return, t = t.sibling;
            }
            switch(o){
                case "forwards":
                    for(i = e.child, o = null; i !== null;)t = i.alternate, t !== null && Ns(t) === null && (o = i), i = i.sibling;
                    i = o, i === null ? (o = e.child, e.child = null) : (o = i.sibling, i.sibling = null), Nu(e, !1, o, i, c, s);
                    break;
                case "backwards":
                case "unstable_legacy-backwards":
                    for(i = null, o = e.child, e.child = null; o !== null;){
                        if (t = o.alternate, t !== null && Ns(t) === null) {
                            e.child = o;
                            break;
                        }
                        t = o.sibling, o.sibling = i, i = o, o = t;
                    }
                    Nu(e, !0, i, null, c, s);
                    break;
                case "together":
                    Nu(e, !1, null, null, void 0, s);
                    break;
                default:
                    e.memoizedState = null;
            }
            return e.child;
        }
        function bn(t, e, i) {
            if (t !== null && (e.dependencies = t.dependencies), Qn |= e.lanes, (i & e.childLanes) === 0) if (t !== null) {
                if (Ia(t, e, i, !1), (i & e.childLanes) === 0) return null;
            } else return null;
            if (t !== null && e.child !== t.child) throw Error(r(153));
            if (e.child !== null) {
                for(t = e.child, i = hn(t, t.pendingProps), e.child = i, i.return = e; t.sibling !== null;)t = t.sibling, i = i.sibling = hn(t, t.pendingProps), i.return = e;
                i.sibling = null;
            }
            return e.child;
        }
        function zu(t, e) {
            return (t.lanes & e) !== 0 ? !0 : (t = t.dependencies, !!(t !== null && Cs(t)));
        }
        function cS(t, e, i) {
            switch(e.tag){
                case 3:
                    Xt(e, e.stateNode.containerInfo), Gn(e, Zt, t.memoizedState.cache), ba();
                    break;
                case 27:
                case 5:
                    Li(e);
                    break;
                case 4:
                    Xt(e, e.stateNode.containerInfo);
                    break;
                case 10:
                    Gn(e, e.type, e.memoizedProps.value);
                    break;
                case 31:
                    if (e.memoizedState !== null) return e.flags |= 128, lu(e), null;
                    break;
                case 13:
                    var s = e.memoizedState;
                    if (s !== null) return s.dehydrated !== null ? (Xn(e), e.flags |= 128, null) : (i & e.child.childLanes) !== 0 ? Km(t, e, i) : (Xn(e), t = bn(t, e, i), t !== null ? t.sibling : null);
                    Xn(e);
                    break;
                case 19:
                    var o = (t.flags & 128) !== 0;
                    if (s = (i & e.childLanes) !== 0, s || (Ia(t, e, i, !1), s = (i & e.childLanes) !== 0), o) {
                        if (s) return Zm(t, e, i);
                        e.flags |= 128;
                    }
                    if (o = e.memoizedState, o !== null && (o.rendering = null, o.tail = null, o.lastEffect = null), J(Kt, Kt.current), s) break;
                    return null;
                case 22:
                    return e.lanes = 0, Hm(t, e, i, e.pendingProps);
                case 24:
                    Gn(e, Zt, t.memoizedState.cache);
            }
            return bn(t, e, i);
        }
        function Qm(t, e, i) {
            if (t !== null) if (t.memoizedProps !== e.pendingProps) Ft = !0;
            else {
                if (!zu(t, i) && (e.flags & 128) === 0) return Ft = !1, cS(t, e, i);
                Ft = (t.flags & 131072) !== 0;
            }
            else Ft = !1, vt && (e.flags & 1048576) !== 0 && wh(e, $i, e.index);
            switch(e.lanes = 0, e.tag){
                case 16:
                    t: {
                        var s = e.pendingProps;
                        if (t = Ea(e.elementType), e.type = t, typeof t == "function") Ho(t) ? (s = _a(t, s), e.tag = 1, e = Ym(null, e, t, s, i)) : (e.tag = 0, e = _u(null, e, t, s, i));
                        else {
                            if (t != null) {
                                var o = t.$$typeof;
                                if (o === P) {
                                    e.tag = 11, e = Vm(null, e, t, s, i);
                                    break t;
                                } else if (o === H) {
                                    e.tag = 14, e = Bm(null, e, t, s, i);
                                    break t;
                                }
                            }
                            throw e = ee(t) || t, Error(r(306, e, ""));
                        }
                    }
                    return e;
                case 0:
                    return _u(t, e, e.type, e.pendingProps, i);
                case 1:
                    return s = e.type, o = _a(s, e.pendingProps), Ym(t, e, s, o, i);
                case 3:
                    t: {
                        if (Xt(e, e.stateNode.containerInfo), t === null) throw Error(r(387));
                        s = e.pendingProps;
                        var c = e.memoizedState;
                        o = c.element, tu(t, e), il(e, s, null, i);
                        var g = e.memoizedState;
                        if (s = g.cache, Gn(e, Zt, s), s !== c.cache && Qo(e, [
                            Zt
                        ], i, !0), al(), s = g.element, c.isDehydrated) if (c = {
                            element: s,
                            isDehydrated: !1,
                            cache: g.cache
                        }, e.updateQueue.baseState = c, e.memoizedState = c, e.flags & 256) {
                            e = Xm(t, e, s, i);
                            break t;
                        } else if (s !== o) {
                            o = Be(Error(r(424)), e), Ji(o), e = Xm(t, e, s, i);
                            break t;
                        } else {
                            switch(t = e.stateNode.containerInfo, t.nodeType){
                                case 9:
                                    t = t.body;
                                    break;
                                default:
                                    t = t.nodeName === "HTML" ? t.ownerDocument.body : t;
                            }
                            for(zt = ke(t.firstChild), se = e, vt = !0, Un = null, Ge = !0, i = Hh(e, null, s, i), e.child = i; i;)i.flags = i.flags & -3 | 4096, i = i.sibling;
                        }
                        else {
                            if (ba(), s === o) {
                                e = bn(t, e, i);
                                break t;
                            }
                            oe(t, e, s, i);
                        }
                        e = e.child;
                    }
                    return e;
                case 26:
                    return Xs(t, e), t === null ? (i = lg(e.type, null, e.pendingProps, null)) ? e.memoizedState = i : vt || (i = e.type, t = e.pendingProps, s = rr(ot.current).createElement(i), s[le] = e, s[ye] = t, ue(s, i, t), ne(s), e.stateNode = s) : e.memoizedState = lg(e.type, t.memoizedProps, e.pendingProps, t.memoizedState), null;
                case 27:
                    return Li(e), t === null && vt && (s = e.stateNode = ng(e.type, e.pendingProps, ot.current), se = e, Ge = !0, o = zt, In(e.type) ? (hc = o, zt = ke(s.firstChild)) : zt = o), oe(t, e, e.pendingProps.children, i), Xs(t, e), t === null && (e.flags |= 4194304), e.child;
                case 5:
                    return t === null && vt && ((o = s = zt) && (s = GS(s, e.type, e.pendingProps, Ge), s !== null ? (e.stateNode = s, se = e, zt = ke(s.firstChild), Ge = !1, o = !0) : o = !1), o || Hn(e)), Li(e), o = e.type, c = e.pendingProps, g = t !== null ? t.memoizedProps : null, s = c.children, oc(o, c) ? s = null : g !== null && oc(o, g) && (e.flags |= 32), e.memoizedState !== null && (o = ru(t, e, eS, null, null, i), Al._currentValue = o), Xs(t, e), oe(t, e, s, i), e.child;
                case 6:
                    return t === null && vt && ((t = i = zt) && (i = qS(i, e.pendingProps, Ge), i !== null ? (e.stateNode = i, se = e, zt = null, t = !0) : t = !1), t || Hn(e)), null;
                case 13:
                    return Km(t, e, i);
                case 4:
                    return Xt(e, e.stateNode.containerInfo), s = e.pendingProps, t === null ? e.child = Ca(e, null, s, i) : oe(t, e, s, i), e.child;
                case 11:
                    return Vm(t, e, e.type, e.pendingProps, i);
                case 7:
                    return oe(t, e, e.pendingProps, i), e.child;
                case 8:
                    return oe(t, e, e.pendingProps.children, i), e.child;
                case 12:
                    return oe(t, e, e.pendingProps.children, i), e.child;
                case 10:
                    return s = e.pendingProps, Gn(e, e.type, s.value), oe(t, e, s.children, i), e.child;
                case 9:
                    return o = e.type._context, s = e.pendingProps.children, Sa(e), o = re(o), s = s(o), e.flags |= 1, oe(t, e, s, i), e.child;
                case 14:
                    return Bm(t, e, e.type, e.pendingProps, i);
                case 15:
                    return Um(t, e, e.type, e.pendingProps, i);
                case 19:
                    return Zm(t, e, i);
                case 31:
                    return uS(t, e, i);
                case 22:
                    return Hm(t, e, i, e.pendingProps);
                case 24:
                    return Sa(e), s = re(Zt), t === null ? (o = Jo(), o === null && (o = jt, c = Fo(), o.pooledCache = c, c.refCount++, c !== null && (o.pooledCacheLanes |= i), o = c), e.memoizedState = {
                        parent: s,
                        cache: o
                    }, Io(e), Gn(e, Zt, o)) : ((t.lanes & i) !== 0 && (tu(t, e), il(e, null, null, i), al()), o = t.memoizedState, c = e.memoizedState, o.parent !== s ? (o = {
                        parent: s,
                        cache: s
                    }, e.memoizedState = o, e.lanes === 0 && (e.memoizedState = e.updateQueue.baseState = o), Gn(e, Zt, s)) : (s = c.cache, Gn(e, Zt, s), s !== o.cache && Qo(e, [
                        Zt
                    ], i, !0))), oe(t, e, e.pendingProps.children, i), e.child;
                case 29:
                    throw e.pendingProps;
            }
            throw Error(r(156, e.tag));
        }
        function xn(t) {
            t.flags |= 4;
        }
        function Lu(t, e, i, s, o) {
            if ((e = (t.mode & 32) !== 0) && (e = !1), e) {
                if (t.flags |= 16777216, (o & 335544128) === o) if (t.stateNode.complete) t.flags |= 8192;
                else if (Sp()) t.flags |= 8192;
                else throw Aa = Ms, Wo;
            } else t.flags &= -16777217;
        }
        function Fm(t, e) {
            if (e.type !== "stylesheet" || (e.state.loading & 4) !== 0) t.flags &= -16777217;
            else if (t.flags |= 16777216, !cg(e)) if (Sp()) t.flags |= 8192;
            else throw Aa = Ms, Wo;
        }
        function Ps(t, e) {
            e !== null && (t.flags |= 4), t.flags & 16384 && (e = t.tag !== 22 ? Rd() : 536870912, t.lanes |= e, fi |= e);
        }
        function cl(t, e) {
            if (!vt) switch(t.tailMode){
                case "hidden":
                    e = t.tail;
                    for(var i = null; e !== null;)e.alternate !== null && (i = e), e = e.sibling;
                    i === null ? t.tail = null : i.sibling = null;
                    break;
                case "collapsed":
                    i = t.tail;
                    for(var s = null; i !== null;)i.alternate !== null && (s = i), i = i.sibling;
                    s === null ? e || t.tail === null ? t.tail = null : t.tail.sibling = null : s.sibling = null;
            }
        }
        function Lt(t) {
            var e = t.alternate !== null && t.alternate.child === t.child, i = 0, s = 0;
            if (e) for(var o = t.child; o !== null;)i |= o.lanes | o.childLanes, s |= o.subtreeFlags & 65011712, s |= o.flags & 65011712, o.return = t, o = o.sibling;
            else for(o = t.child; o !== null;)i |= o.lanes | o.childLanes, s |= o.subtreeFlags, s |= o.flags, o.return = t, o = o.sibling;
            return t.subtreeFlags |= s, t.childLanes = i, e;
        }
        function fS(t, e, i) {
            var s = e.pendingProps;
            switch(Yo(e), e.tag){
                case 16:
                case 15:
                case 0:
                case 11:
                case 7:
                case 8:
                case 12:
                case 9:
                case 14:
                    return Lt(e), null;
                case 1:
                    return Lt(e), null;
                case 3:
                    return i = e.stateNode, s = null, t !== null && (s = t.memoizedState.cache), e.memoizedState.cache !== s && (e.flags |= 2048), gn(Zt), Dt(), i.pendingContext && (i.context = i.pendingContext, i.pendingContext = null), (t === null || t.child === null) && (Wa(e) ? xn(e) : t === null || t.memoizedState.isDehydrated && (e.flags & 256) === 0 || (e.flags |= 1024, Ko())), Lt(e), null;
                case 26:
                    var o = e.type, c = e.memoizedState;
                    return t === null ? (xn(e), c !== null ? (Lt(e), Fm(e, c)) : (Lt(e), Lu(e, o, null, s, i))) : c ? c !== t.memoizedState ? (xn(e), Lt(e), Fm(e, c)) : (Lt(e), e.flags &= -16777217) : (t = t.memoizedProps, t !== s && xn(e), Lt(e), Lu(e, o, t, s, i)), null;
                case 27:
                    if (as(e), i = ot.current, o = e.type, t !== null && e.stateNode != null) t.memoizedProps !== s && xn(e);
                    else {
                        if (!s) {
                            if (e.stateNode === null) throw Error(r(166));
                            return Lt(e), null;
                        }
                        t = W.current, Wa(e) ? Rh(e) : (t = ng(o, s, i), e.stateNode = t, xn(e));
                    }
                    return Lt(e), null;
                case 5:
                    if (as(e), o = e.type, t !== null && e.stateNode != null) t.memoizedProps !== s && xn(e);
                    else {
                        if (!s) {
                            if (e.stateNode === null) throw Error(r(166));
                            return Lt(e), null;
                        }
                        if (c = W.current, Wa(e)) Rh(e);
                        else {
                            var g = rr(ot.current);
                            switch(c){
                                case 1:
                                    c = g.createElementNS("http://www.w3.org/2000/svg", o);
                                    break;
                                case 2:
                                    c = g.createElementNS("http://www.w3.org/1998/Math/MathML", o);
                                    break;
                                default:
                                    switch(o){
                                        case "svg":
                                            c = g.createElementNS("http://www.w3.org/2000/svg", o);
                                            break;
                                        case "math":
                                            c = g.createElementNS("http://www.w3.org/1998/Math/MathML", o);
                                            break;
                                        case "script":
                                            c = g.createElement("div"), c.innerHTML = "<script><\/script>", c = c.removeChild(c.firstChild);
                                            break;
                                        case "select":
                                            c = typeof s.is == "string" ? g.createElement("select", {
                                                is: s.is
                                            }) : g.createElement("select"), s.multiple ? c.multiple = !0 : s.size && (c.size = s.size);
                                            break;
                                        default:
                                            c = typeof s.is == "string" ? g.createElement(o, {
                                                is: s.is
                                            }) : g.createElement(o);
                                    }
                            }
                            c[le] = e, c[ye] = s;
                            t: for(g = e.child; g !== null;){
                                if (g.tag === 5 || g.tag === 6) c.appendChild(g.stateNode);
                                else if (g.tag !== 4 && g.tag !== 27 && g.child !== null) {
                                    g.child.return = g, g = g.child;
                                    continue;
                                }
                                if (g === e) break t;
                                for(; g.sibling === null;){
                                    if (g.return === null || g.return === e) break t;
                                    g = g.return;
                                }
                                g.sibling.return = g.return, g = g.sibling;
                            }
                            e.stateNode = c;
                            t: switch(ue(c, o, s), o){
                                case "button":
                                case "input":
                                case "select":
                                case "textarea":
                                    s = !!s.autoFocus;
                                    break t;
                                case "img":
                                    s = !0;
                                    break t;
                                default:
                                    s = !1;
                            }
                            s && xn(e);
                        }
                    }
                    return Lt(e), Lu(e, e.type, t === null ? null : t.memoizedProps, e.pendingProps, i), null;
                case 6:
                    if (t && e.stateNode != null) t.memoizedProps !== s && xn(e);
                    else {
                        if (typeof s != "string" && e.stateNode === null) throw Error(r(166));
                        if (t = ot.current, Wa(e)) {
                            if (t = e.stateNode, i = e.memoizedProps, s = null, o = se, o !== null) switch(o.tag){
                                case 27:
                                case 5:
                                    s = o.memoizedProps;
                            }
                            t[le] = e, t = !!(t.nodeValue === i || s !== null && s.suppressHydrationWarning === !0 || Kp(t.nodeValue, i)), t || Hn(e, !0);
                        } else t = rr(t).createTextNode(s), t[le] = e, e.stateNode = t;
                    }
                    return Lt(e), null;
                case 31:
                    if (i = e.memoizedState, t === null || t.memoizedState !== null) {
                        if (s = Wa(e), i !== null) {
                            if (t === null) {
                                if (!s) throw Error(r(318));
                                if (t = e.memoizedState, t = t !== null ? t.dehydrated : null, !t) throw Error(r(557));
                                t[le] = e;
                            } else ba(), (e.flags & 128) === 0 && (e.memoizedState = null), e.flags |= 4;
                            Lt(e), t = !1;
                        } else i = Ko(), t !== null && t.memoizedState !== null && (t.memoizedState.hydrationErrors = i), t = !0;
                        if (!t) return e.flags & 256 ? (De(e), e) : (De(e), null);
                        if ((e.flags & 128) !== 0) throw Error(r(558));
                    }
                    return Lt(e), null;
                case 13:
                    if (s = e.memoizedState, t === null || t.memoizedState !== null && t.memoizedState.dehydrated !== null) {
                        if (o = Wa(e), s !== null && s.dehydrated !== null) {
                            if (t === null) {
                                if (!o) throw Error(r(318));
                                if (o = e.memoizedState, o = o !== null ? o.dehydrated : null, !o) throw Error(r(317));
                                o[le] = e;
                            } else ba(), (e.flags & 128) === 0 && (e.memoizedState = null), e.flags |= 4;
                            Lt(e), o = !1;
                        } else o = Ko(), t !== null && t.memoizedState !== null && (t.memoizedState.hydrationErrors = o), o = !0;
                        if (!o) return e.flags & 256 ? (De(e), e) : (De(e), null);
                    }
                    return De(e), (e.flags & 128) !== 0 ? (e.lanes = i, e) : (i = s !== null, t = t !== null && t.memoizedState !== null, i && (s = e.child, o = null, s.alternate !== null && s.alternate.memoizedState !== null && s.alternate.memoizedState.cachePool !== null && (o = s.alternate.memoizedState.cachePool.pool), c = null, s.memoizedState !== null && s.memoizedState.cachePool !== null && (c = s.memoizedState.cachePool.pool), c !== o && (s.flags |= 2048)), i !== t && i && (e.child.flags |= 8192), Ps(e, e.updateQueue), Lt(e), null);
                case 4:
                    return Dt(), t === null && ac(e.stateNode.containerInfo), Lt(e), null;
                case 10:
                    return gn(e.type), Lt(e), null;
                case 19:
                    if (Y(Kt), s = e.memoizedState, s === null) return Lt(e), null;
                    if (o = (e.flags & 128) !== 0, c = s.rendering, c === null) if (o) cl(s, !1);
                    else {
                        if (qt !== 0 || t !== null && (t.flags & 128) !== 0) for(t = e.child; t !== null;){
                            if (c = Ns(t), c !== null) {
                                for(e.flags |= 128, cl(s, !1), t = c.updateQueue, e.updateQueue = t, Ps(e, t), e.subtreeFlags = 0, t = i, i = e.child; i !== null;)Eh(i, t), i = i.sibling;
                                return J(Kt, Kt.current & 1 | 2), vt && mn(e, s.treeForkCount), e.child;
                            }
                            t = t.sibling;
                        }
                        s.tail !== null && Ae() > Js && (e.flags |= 128, o = !0, cl(s, !1), e.lanes = 4194304);
                    }
                    else {
                        if (!o) if (t = Ns(c), t !== null) {
                            if (e.flags |= 128, o = !0, t = t.updateQueue, e.updateQueue = t, Ps(e, t), cl(s, !0), s.tail === null && s.tailMode === "hidden" && !c.alternate && !vt) return Lt(e), null;
                        } else 2 * Ae() - s.renderingStartTime > Js && i !== 536870912 && (e.flags |= 128, o = !0, cl(s, !1), e.lanes = 4194304);
                        s.isBackwards ? (c.sibling = e.child, e.child = c) : (t = s.last, t !== null ? t.sibling = c : e.child = c, s.last = c);
                    }
                    return s.tail !== null ? (t = s.tail, s.rendering = t, s.tail = t.sibling, s.renderingStartTime = Ae(), t.sibling = null, i = Kt.current, J(Kt, o ? i & 1 | 2 : i & 1), vt && mn(e, s.treeForkCount), t) : (Lt(e), null);
                case 22:
                case 23:
                    return De(e), iu(), s = e.memoizedState !== null, t !== null ? t.memoizedState !== null !== s && (e.flags |= 8192) : s && (e.flags |= 8192), s ? (i & 536870912) !== 0 && (e.flags & 128) === 0 && (Lt(e), e.subtreeFlags & 6 && (e.flags |= 8192)) : Lt(e), i = e.updateQueue, i !== null && Ps(e, i.retryQueue), i = null, t !== null && t.memoizedState !== null && t.memoizedState.cachePool !== null && (i = t.memoizedState.cachePool.pool), s = null, e.memoizedState !== null && e.memoizedState.cachePool !== null && (s = e.memoizedState.cachePool.pool), s !== i && (e.flags |= 2048), t !== null && Y(Ta), null;
                case 24:
                    return i = null, t !== null && (i = t.memoizedState.cache), e.memoizedState.cache !== i && (e.flags |= 2048), gn(Zt), Lt(e), null;
                case 25:
                    return null;
                case 30:
                    return null;
            }
            throw Error(r(156, e.tag));
        }
        function dS(t, e) {
            switch(Yo(e), e.tag){
                case 1:
                    return t = e.flags, t & 65536 ? (e.flags = t & -65537 | 128, e) : null;
                case 3:
                    return gn(Zt), Dt(), t = e.flags, (t & 65536) !== 0 && (t & 128) === 0 ? (e.flags = t & -65537 | 128, e) : null;
                case 26:
                case 27:
                case 5:
                    return as(e), null;
                case 31:
                    if (e.memoizedState !== null) {
                        if (De(e), e.alternate === null) throw Error(r(340));
                        ba();
                    }
                    return t = e.flags, t & 65536 ? (e.flags = t & -65537 | 128, e) : null;
                case 13:
                    if (De(e), t = e.memoizedState, t !== null && t.dehydrated !== null) {
                        if (e.alternate === null) throw Error(r(340));
                        ba();
                    }
                    return t = e.flags, t & 65536 ? (e.flags = t & -65537 | 128, e) : null;
                case 19:
                    return Y(Kt), null;
                case 4:
                    return Dt(), null;
                case 10:
                    return gn(e.type), null;
                case 22:
                case 23:
                    return De(e), iu(), t !== null && Y(Ta), t = e.flags, t & 65536 ? (e.flags = t & -65537 | 128, e) : null;
                case 24:
                    return gn(Zt), null;
                case 25:
                    return null;
                default:
                    return null;
            }
        }
        function $m(t, e) {
            switch(Yo(e), e.tag){
                case 3:
                    gn(Zt), Dt();
                    break;
                case 26:
                case 27:
                case 5:
                    as(e);
                    break;
                case 4:
                    Dt();
                    break;
                case 31:
                    e.memoizedState !== null && De(e);
                    break;
                case 13:
                    De(e);
                    break;
                case 19:
                    Y(Kt);
                    break;
                case 10:
                    gn(e.type);
                    break;
                case 22:
                case 23:
                    De(e), iu(), t !== null && Y(Ta);
                    break;
                case 24:
                    gn(Zt);
            }
        }
        function fl(t, e) {
            try {
                var i = e.updateQueue, s = i !== null ? i.lastEffect : null;
                if (s !== null) {
                    var o = s.next;
                    i = o;
                    do {
                        if ((i.tag & t) === t) {
                            s = void 0;
                            var c = i.create, g = i.inst;
                            s = c(), g.destroy = s;
                        }
                        i = i.next;
                    }while (i !== o);
                }
            } catch (b) {
                Ct(e, e.return, b);
            }
        }
        function Pn(t, e, i) {
            try {
                var s = e.updateQueue, o = s !== null ? s.lastEffect : null;
                if (o !== null) {
                    var c = o.next;
                    s = c;
                    do {
                        if ((s.tag & t) === t) {
                            var g = s.inst, b = g.destroy;
                            if (b !== void 0) {
                                g.destroy = void 0, o = e;
                                var C = i, O = b;
                                try {
                                    O();
                                } catch (q) {
                                    Ct(o, C, q);
                                }
                            }
                        }
                        s = s.next;
                    }while (s !== c);
                }
            } catch (q) {
                Ct(e, e.return, q);
            }
        }
        function Jm(t) {
            var e = t.updateQueue;
            if (e !== null) {
                var i = t.stateNode;
                try {
                    qh(e, i);
                } catch (s) {
                    Ct(t, t.return, s);
                }
            }
        }
        function Wm(t, e, i) {
            i.props = _a(t.type, t.memoizedProps), i.state = t.memoizedState;
            try {
                i.componentWillUnmount();
            } catch (s) {
                Ct(t, e, s);
            }
        }
        function dl(t, e) {
            try {
                var i = t.ref;
                if (i !== null) {
                    switch(t.tag){
                        case 26:
                        case 27:
                        case 5:
                            var s = t.stateNode;
                            break;
                        case 30:
                            s = t.stateNode;
                            break;
                        default:
                            s = t.stateNode;
                    }
                    typeof i == "function" ? t.refCleanup = i(s) : i.current = s;
                }
            } catch (o) {
                Ct(t, e, o);
            }
        }
        function nn(t, e) {
            var i = t.ref, s = t.refCleanup;
            if (i !== null) if (typeof s == "function") try {
                s();
            } catch (o) {
                Ct(t, e, o);
            } finally{
                t.refCleanup = null, t = t.alternate, t != null && (t.refCleanup = null);
            }
            else if (typeof i == "function") try {
                i(null);
            } catch (o) {
                Ct(t, e, o);
            }
            else i.current = null;
        }
        function Im(t) {
            var e = t.type, i = t.memoizedProps, s = t.stateNode;
            try {
                t: switch(e){
                    case "button":
                    case "input":
                    case "select":
                    case "textarea":
                        i.autoFocus && s.focus();
                        break t;
                    case "img":
                        i.src ? s.src = i.src : i.srcSet && (s.srcset = i.srcSet);
                }
            } catch (o) {
                Ct(t, t.return, o);
            }
        }
        function Vu(t, e, i) {
            try {
                var s = t.stateNode;
                zS(s, t.type, i, e), s[ye] = e;
            } catch (o) {
                Ct(t, t.return, o);
            }
        }
        function tp(t) {
            return t.tag === 5 || t.tag === 3 || t.tag === 26 || t.tag === 27 && In(t.type) || t.tag === 4;
        }
        function Bu(t) {
            t: for(;;){
                for(; t.sibling === null;){
                    if (t.return === null || tp(t.return)) return null;
                    t = t.return;
                }
                for(t.sibling.return = t.return, t = t.sibling; t.tag !== 5 && t.tag !== 6 && t.tag !== 18;){
                    if (t.tag === 27 && In(t.type) || t.flags & 2 || t.child === null || t.tag === 4) continue t;
                    t.child.return = t, t = t.child;
                }
                if (!(t.flags & 2)) return t.stateNode;
            }
        }
        function Uu(t, e, i) {
            var s = t.tag;
            if (s === 5 || s === 6) t = t.stateNode, e ? (i.nodeType === 9 ? i.body : i.nodeName === "HTML" ? i.ownerDocument.body : i).insertBefore(t, e) : (e = i.nodeType === 9 ? i.body : i.nodeName === "HTML" ? i.ownerDocument.body : i, e.appendChild(t), i = i._reactRootContainer, i != null || e.onclick !== null || (e.onclick = fn));
            else if (s !== 4 && (s === 27 && In(t.type) && (i = t.stateNode, e = null), t = t.child, t !== null)) for(Uu(t, e, i), t = t.sibling; t !== null;)Uu(t, e, i), t = t.sibling;
        }
        function Zs(t, e, i) {
            var s = t.tag;
            if (s === 5 || s === 6) t = t.stateNode, e ? i.insertBefore(t, e) : i.appendChild(t);
            else if (s !== 4 && (s === 27 && In(t.type) && (i = t.stateNode), t = t.child, t !== null)) for(Zs(t, e, i), t = t.sibling; t !== null;)Zs(t, e, i), t = t.sibling;
        }
        function ep(t) {
            var e = t.stateNode, i = t.memoizedProps;
            try {
                for(var s = t.type, o = e.attributes; o.length;)e.removeAttributeNode(o[0]);
                ue(e, s, i), e[le] = t, e[ye] = i;
            } catch (c) {
                Ct(t, t.return, c);
            }
        }
        var Sn = !1, $t = !1, Hu = !1, np = typeof WeakSet == "function" ? WeakSet : Set, ae = null;
        function hS(t, e) {
            if (t = t.containerInfo, sc = mr, t = mh(t), Oo(t)) {
                if ("selectionStart" in t) var i = {
                    start: t.selectionStart,
                    end: t.selectionEnd
                };
                else t: {
                    i = (i = t.ownerDocument) && i.defaultView || window;
                    var s = i.getSelection && i.getSelection();
                    if (s && s.rangeCount !== 0) {
                        i = s.anchorNode;
                        var o = s.anchorOffset, c = s.focusNode;
                        s = s.focusOffset;
                        try {
                            i.nodeType, c.nodeType;
                        } catch  {
                            i = null;
                            break t;
                        }
                        var g = 0, b = -1, C = -1, O = 0, q = 0, K = t, N = null;
                        e: for(;;){
                            for(var L; K !== i || o !== 0 && K.nodeType !== 3 || (b = g + o), K !== c || s !== 0 && K.nodeType !== 3 || (C = g + s), K.nodeType === 3 && (g += K.nodeValue.length), (L = K.firstChild) !== null;)N = K, K = L;
                            for(;;){
                                if (K === t) break e;
                                if (N === i && ++O === o && (b = g), N === c && ++q === s && (C = g), (L = K.nextSibling) !== null) break;
                                K = N, N = K.parentNode;
                            }
                            K = L;
                        }
                        i = b === -1 || C === -1 ? null : {
                            start: b,
                            end: C
                        };
                    } else i = null;
                }
                i = i || {
                    start: 0,
                    end: 0
                };
            } else i = null;
            for(rc = {
                focusedElem: t,
                selectionRange: i
            }, mr = !1, ae = e; ae !== null;)if (e = ae, t = e.child, (e.subtreeFlags & 1028) !== 0 && t !== null) t.return = e, ae = t;
            else for(; ae !== null;){
                switch(e = ae, c = e.alternate, t = e.flags, e.tag){
                    case 0:
                        if ((t & 4) !== 0 && (t = e.updateQueue, t = t !== null ? t.events : null, t !== null)) for(i = 0; i < t.length; i++)o = t[i], o.ref.impl = o.nextImpl;
                        break;
                    case 11:
                    case 15:
                        break;
                    case 1:
                        if ((t & 1024) !== 0 && c !== null) {
                            t = void 0, i = e, o = c.memoizedProps, c = c.memoizedState, s = i.stateNode;
                            try {
                                var I = _a(i.type, o);
                                t = s.getSnapshotBeforeUpdate(I, c), s.__reactInternalSnapshotBeforeUpdate = t;
                            } catch (at) {
                                Ct(i, i.return, at);
                            }
                        }
                        break;
                    case 3:
                        if ((t & 1024) !== 0) {
                            if (t = e.stateNode.containerInfo, i = t.nodeType, i === 9) cc(t);
                            else if (i === 1) switch(t.nodeName){
                                case "HEAD":
                                case "HTML":
                                case "BODY":
                                    cc(t);
                                    break;
                                default:
                                    t.textContent = "";
                            }
                        }
                        break;
                    case 5:
                    case 26:
                    case 27:
                    case 6:
                    case 4:
                    case 17:
                        break;
                    default:
                        if ((t & 1024) !== 0) throw Error(r(163));
                }
                if (t = e.sibling, t !== null) {
                    t.return = e.return, ae = t;
                    break;
                }
                ae = e.return;
            }
        }
        function ap(t, e, i) {
            var s = i.flags;
            switch(i.tag){
                case 0:
                case 11:
                case 15:
                    En(t, i), s & 4 && fl(5, i);
                    break;
                case 1:
                    if (En(t, i), s & 4) if (t = i.stateNode, e === null) try {
                        t.componentDidMount();
                    } catch (g) {
                        Ct(i, i.return, g);
                    }
                    else {
                        var o = _a(i.type, e.memoizedProps);
                        e = e.memoizedState;
                        try {
                            t.componentDidUpdate(o, e, t.__reactInternalSnapshotBeforeUpdate);
                        } catch (g) {
                            Ct(i, i.return, g);
                        }
                    }
                    s & 64 && Jm(i), s & 512 && dl(i, i.return);
                    break;
                case 3:
                    if (En(t, i), s & 64 && (t = i.updateQueue, t !== null)) {
                        if (e = null, i.child !== null) switch(i.child.tag){
                            case 27:
                            case 5:
                                e = i.child.stateNode;
                                break;
                            case 1:
                                e = i.child.stateNode;
                        }
                        try {
                            qh(t, e);
                        } catch (g) {
                            Ct(i, i.return, g);
                        }
                    }
                    break;
                case 27:
                    e === null && s & 4 && ep(i);
                case 26:
                case 5:
                    En(t, i), e === null && s & 4 && Im(i), s & 512 && dl(i, i.return);
                    break;
                case 12:
                    En(t, i);
                    break;
                case 31:
                    En(t, i), s & 4 && sp(t, i);
                    break;
                case 13:
                    En(t, i), s & 4 && rp(t, i), s & 64 && (t = i.memoizedState, t !== null && (t = t.dehydrated, t !== null && (i = TS.bind(null, i), kS(t, i))));
                    break;
                case 22:
                    if (s = i.memoizedState !== null || Sn, !s) {
                        e = e !== null && e.memoizedState !== null || $t, o = Sn;
                        var c = $t;
                        Sn = s, ($t = e) && !c ? An(t, i, (i.subtreeFlags & 8772) !== 0) : En(t, i), Sn = o, $t = c;
                    }
                    break;
                case 30:
                    break;
                default:
                    En(t, i);
            }
        }
        function ip(t) {
            var e = t.alternate;
            e !== null && (t.alternate = null, ip(e)), t.child = null, t.deletions = null, t.sibling = null, t.tag === 5 && (e = t.stateNode, e !== null && po(e)), t.stateNode = null, t.return = null, t.dependencies = null, t.memoizedProps = null, t.memoizedState = null, t.pendingProps = null, t.stateNode = null, t.updateQueue = null;
        }
        var Bt = null, be = !1;
        function Tn(t, e, i) {
            for(i = i.child; i !== null;)lp(t, e, i), i = i.sibling;
        }
        function lp(t, e, i) {
            if (Ce && typeof Ce.onCommitFiberUnmount == "function") try {
                Ce.onCommitFiberUnmount(Vi, i);
            } catch  {}
            switch(i.tag){
                case 26:
                    $t || nn(i, e), Tn(t, e, i), i.memoizedState ? i.memoizedState.count-- : i.stateNode && (i = i.stateNode, i.parentNode.removeChild(i));
                    break;
                case 27:
                    $t || nn(i, e);
                    var s = Bt, o = be;
                    In(i.type) && (Bt = i.stateNode, be = !1), Tn(t, e, i), Sl(i.stateNode), Bt = s, be = o;
                    break;
                case 5:
                    $t || nn(i, e);
                case 6:
                    if (s = Bt, o = be, Bt = null, Tn(t, e, i), Bt = s, be = o, Bt !== null) if (be) try {
                        (Bt.nodeType === 9 ? Bt.body : Bt.nodeName === "HTML" ? Bt.ownerDocument.body : Bt).removeChild(i.stateNode);
                    } catch (c) {
                        Ct(i, e, c);
                    }
                    else try {
                        Bt.removeChild(i.stateNode);
                    } catch (c) {
                        Ct(i, e, c);
                    }
                    break;
                case 18:
                    Bt !== null && (be ? (t = Bt, Jp(t.nodeType === 9 ? t.body : t.nodeName === "HTML" ? t.ownerDocument.body : t, i.stateNode), bi(t)) : Jp(Bt, i.stateNode));
                    break;
                case 4:
                    s = Bt, o = be, Bt = i.stateNode.containerInfo, be = !0, Tn(t, e, i), Bt = s, be = o;
                    break;
                case 0:
                case 11:
                case 14:
                case 15:
                    Pn(2, i, e), $t || Pn(4, i, e), Tn(t, e, i);
                    break;
                case 1:
                    $t || (nn(i, e), s = i.stateNode, typeof s.componentWillUnmount == "function" && Wm(i, e, s)), Tn(t, e, i);
                    break;
                case 21:
                    Tn(t, e, i);
                    break;
                case 22:
                    $t = (s = $t) || i.memoizedState !== null, Tn(t, e, i), $t = s;
                    break;
                default:
                    Tn(t, e, i);
            }
        }
        function sp(t, e) {
            if (e.memoizedState === null && (t = e.alternate, t !== null && (t = t.memoizedState, t !== null))) {
                t = t.dehydrated;
                try {
                    bi(t);
                } catch (i) {
                    Ct(e, e.return, i);
                }
            }
        }
        function rp(t, e) {
            if (e.memoizedState === null && (t = e.alternate, t !== null && (t = t.memoizedState, t !== null && (t = t.dehydrated, t !== null)))) try {
                bi(t);
            } catch (i) {
                Ct(e, e.return, i);
            }
        }
        function mS(t) {
            switch(t.tag){
                case 31:
                case 13:
                case 19:
                    var e = t.stateNode;
                    return e === null && (e = t.stateNode = new np), e;
                case 22:
                    return t = t.stateNode, e = t._retryCache, e === null && (e = t._retryCache = new np), e;
                default:
                    throw Error(r(435, t.tag));
            }
        }
        function Qs(t, e) {
            var i = mS(t);
            e.forEach(function(s) {
                if (!i.has(s)) {
                    i.add(s);
                    var o = ES.bind(null, t, s);
                    s.then(o, o);
                }
            });
        }
        function xe(t, e) {
            var i = e.deletions;
            if (i !== null) for(var s = 0; s < i.length; s++){
                var o = i[s], c = t, g = e, b = g;
                t: for(; b !== null;){
                    switch(b.tag){
                        case 27:
                            if (In(b.type)) {
                                Bt = b.stateNode, be = !1;
                                break t;
                            }
                            break;
                        case 5:
                            Bt = b.stateNode, be = !1;
                            break t;
                        case 3:
                        case 4:
                            Bt = b.stateNode.containerInfo, be = !0;
                            break t;
                    }
                    b = b.return;
                }
                if (Bt === null) throw Error(r(160));
                lp(c, g, o), Bt = null, be = !1, c = o.alternate, c !== null && (c.return = null), o.return = null;
            }
            if (e.subtreeFlags & 13886) for(e = e.child; e !== null;)op(e, t), e = e.sibling;
        }
        var Fe = null;
        function op(t, e) {
            var i = t.alternate, s = t.flags;
            switch(t.tag){
                case 0:
                case 11:
                case 14:
                case 15:
                    xe(e, t), Se(t), s & 4 && (Pn(3, t, t.return), fl(3, t), Pn(5, t, t.return));
                    break;
                case 1:
                    xe(e, t), Se(t), s & 512 && ($t || i === null || nn(i, i.return)), s & 64 && Sn && (t = t.updateQueue, t !== null && (s = t.callbacks, s !== null && (i = t.shared.hiddenCallbacks, t.shared.hiddenCallbacks = i === null ? s : i.concat(s))));
                    break;
                case 26:
                    var o = Fe;
                    if (xe(e, t), Se(t), s & 512 && ($t || i === null || nn(i, i.return)), s & 4) {
                        var c = i !== null ? i.memoizedState : null;
                        if (s = t.memoizedState, i === null) if (s === null) if (t.stateNode === null) {
                            t: {
                                s = t.type, i = t.memoizedProps, o = o.ownerDocument || o;
                                e: switch(s){
                                    case "title":
                                        c = o.getElementsByTagName("title")[0], (!c || c[Hi] || c[le] || c.namespaceURI === "http://www.w3.org/2000/svg" || c.hasAttribute("itemprop")) && (c = o.createElement(s), o.head.insertBefore(c, o.querySelector("head > title"))), ue(c, s, i), c[le] = t, ne(c), s = c;
                                        break t;
                                    case "link":
                                        var g = og("link", "href", o).get(s + (i.href || ""));
                                        if (g) {
                                            for(var b = 0; b < g.length; b++)if (c = g[b], c.getAttribute("href") === (i.href == null || i.href === "" ? null : i.href) && c.getAttribute("rel") === (i.rel == null ? null : i.rel) && c.getAttribute("title") === (i.title == null ? null : i.title) && c.getAttribute("crossorigin") === (i.crossOrigin == null ? null : i.crossOrigin)) {
                                                g.splice(b, 1);
                                                break e;
                                            }
                                        }
                                        c = o.createElement(s), ue(c, s, i), o.head.appendChild(c);
                                        break;
                                    case "meta":
                                        if (g = og("meta", "content", o).get(s + (i.content || ""))) {
                                            for(b = 0; b < g.length; b++)if (c = g[b], c.getAttribute("content") === (i.content == null ? null : "" + i.content) && c.getAttribute("name") === (i.name == null ? null : i.name) && c.getAttribute("property") === (i.property == null ? null : i.property) && c.getAttribute("http-equiv") === (i.httpEquiv == null ? null : i.httpEquiv) && c.getAttribute("charset") === (i.charSet == null ? null : i.charSet)) {
                                                g.splice(b, 1);
                                                break e;
                                            }
                                        }
                                        c = o.createElement(s), ue(c, s, i), o.head.appendChild(c);
                                        break;
                                    default:
                                        throw Error(r(468, s));
                                }
                                c[le] = t, ne(c), s = c;
                            }
                            t.stateNode = s;
                        } else ug(o, t.type, t.stateNode);
                        else t.stateNode = rg(o, s, t.memoizedProps);
                        else c !== s ? (c === null ? i.stateNode !== null && (i = i.stateNode, i.parentNode.removeChild(i)) : c.count--, s === null ? ug(o, t.type, t.stateNode) : rg(o, s, t.memoizedProps)) : s === null && t.stateNode !== null && Vu(t, t.memoizedProps, i.memoizedProps);
                    }
                    break;
                case 27:
                    xe(e, t), Se(t), s & 512 && ($t || i === null || nn(i, i.return)), i !== null && s & 4 && Vu(t, t.memoizedProps, i.memoizedProps);
                    break;
                case 5:
                    if (xe(e, t), Se(t), s & 512 && ($t || i === null || nn(i, i.return)), t.flags & 32) {
                        o = t.stateNode;
                        try {
                            ka(o, "");
                        } catch (I) {
                            Ct(t, t.return, I);
                        }
                    }
                    s & 4 && t.stateNode != null && (o = t.memoizedProps, Vu(t, o, i !== null ? i.memoizedProps : o)), s & 1024 && (Hu = !0);
                    break;
                case 6:
                    if (xe(e, t), Se(t), s & 4) {
                        if (t.stateNode === null) throw Error(r(162));
                        s = t.memoizedProps, i = t.stateNode;
                        try {
                            i.nodeValue = s;
                        } catch (I) {
                            Ct(t, t.return, I);
                        }
                    }
                    break;
                case 3:
                    if (cr = null, o = Fe, Fe = or(e.containerInfo), xe(e, t), Fe = o, Se(t), s & 4 && i !== null && i.memoizedState.isDehydrated) try {
                        bi(e.containerInfo);
                    } catch (I) {
                        Ct(t, t.return, I);
                    }
                    Hu && (Hu = !1, up(t));
                    break;
                case 4:
                    s = Fe, Fe = or(t.stateNode.containerInfo), xe(e, t), Se(t), Fe = s;
                    break;
                case 12:
                    xe(e, t), Se(t);
                    break;
                case 31:
                    xe(e, t), Se(t), s & 4 && (s = t.updateQueue, s !== null && (t.updateQueue = null, Qs(t, s)));
                    break;
                case 13:
                    xe(e, t), Se(t), t.child.flags & 8192 && t.memoizedState !== null != (i !== null && i.memoizedState !== null) && ($s = Ae()), s & 4 && (s = t.updateQueue, s !== null && (t.updateQueue = null, Qs(t, s)));
                    break;
                case 22:
                    o = t.memoizedState !== null;
                    var C = i !== null && i.memoizedState !== null, O = Sn, q = $t;
                    if (Sn = O || o, $t = q || C, xe(e, t), $t = q, Sn = O, Se(t), s & 8192) t: for(e = t.stateNode, e._visibility = o ? e._visibility & -2 : e._visibility | 1, o && (i === null || C || Sn || $t || Ra(t)), i = null, e = t;;){
                        if (e.tag === 5 || e.tag === 26) {
                            if (i === null) {
                                C = i = e;
                                try {
                                    if (c = C.stateNode, o) g = c.style, typeof g.setProperty == "function" ? g.setProperty("display", "none", "important") : g.display = "none";
                                    else {
                                        b = C.stateNode;
                                        var K = C.memoizedProps.style, N = K != null && K.hasOwnProperty("display") ? K.display : null;
                                        b.style.display = N == null || typeof N == "boolean" ? "" : ("" + N).trim();
                                    }
                                } catch (I) {
                                    Ct(C, C.return, I);
                                }
                            }
                        } else if (e.tag === 6) {
                            if (i === null) {
                                C = e;
                                try {
                                    C.stateNode.nodeValue = o ? "" : C.memoizedProps;
                                } catch (I) {
                                    Ct(C, C.return, I);
                                }
                            }
                        } else if (e.tag === 18) {
                            if (i === null) {
                                C = e;
                                try {
                                    var L = C.stateNode;
                                    o ? Wp(L, !0) : Wp(C.stateNode, !1);
                                } catch (I) {
                                    Ct(C, C.return, I);
                                }
                            }
                        } else if ((e.tag !== 22 && e.tag !== 23 || e.memoizedState === null || e === t) && e.child !== null) {
                            e.child.return = e, e = e.child;
                            continue;
                        }
                        if (e === t) break t;
                        for(; e.sibling === null;){
                            if (e.return === null || e.return === t) break t;
                            i === e && (i = null), e = e.return;
                        }
                        i === e && (i = null), e.sibling.return = e.return, e = e.sibling;
                    }
                    s & 4 && (s = t.updateQueue, s !== null && (i = s.retryQueue, i !== null && (s.retryQueue = null, Qs(t, i))));
                    break;
                case 19:
                    xe(e, t), Se(t), s & 4 && (s = t.updateQueue, s !== null && (t.updateQueue = null, Qs(t, s)));
                    break;
                case 30:
                    break;
                case 21:
                    break;
                default:
                    xe(e, t), Se(t);
            }
        }
        function Se(t) {
            var e = t.flags;
            if (e & 2) {
                try {
                    for(var i, s = t.return; s !== null;){
                        if (tp(s)) {
                            i = s;
                            break;
                        }
                        s = s.return;
                    }
                    if (i == null) throw Error(r(160));
                    switch(i.tag){
                        case 27:
                            var o = i.stateNode, c = Bu(t);
                            Zs(t, c, o);
                            break;
                        case 5:
                            var g = i.stateNode;
                            i.flags & 32 && (ka(g, ""), i.flags &= -33);
                            var b = Bu(t);
                            Zs(t, b, g);
                            break;
                        case 3:
                        case 4:
                            var C = i.stateNode.containerInfo, O = Bu(t);
                            Uu(t, O, C);
                            break;
                        default:
                            throw Error(r(161));
                    }
                } catch (q) {
                    Ct(t, t.return, q);
                }
                t.flags &= -3;
            }
            e & 4096 && (t.flags &= -4097);
        }
        function up(t) {
            if (t.subtreeFlags & 1024) for(t = t.child; t !== null;){
                var e = t;
                up(e), e.tag === 5 && e.flags & 1024 && e.stateNode.reset(), t = t.sibling;
            }
        }
        function En(t, e) {
            if (e.subtreeFlags & 8772) for(e = e.child; e !== null;)ap(t, e.alternate, e), e = e.sibling;
        }
        function Ra(t) {
            for(t = t.child; t !== null;){
                var e = t;
                switch(e.tag){
                    case 0:
                    case 11:
                    case 14:
                    case 15:
                        Pn(4, e, e.return), Ra(e);
                        break;
                    case 1:
                        nn(e, e.return);
                        var i = e.stateNode;
                        typeof i.componentWillUnmount == "function" && Wm(e, e.return, i), Ra(e);
                        break;
                    case 27:
                        Sl(e.stateNode);
                    case 26:
                    case 5:
                        nn(e, e.return), Ra(e);
                        break;
                    case 22:
                        e.memoizedState === null && Ra(e);
                        break;
                    case 30:
                        Ra(e);
                        break;
                    default:
                        Ra(e);
                }
                t = t.sibling;
            }
        }
        function An(t, e, i) {
            for(i = i && (e.subtreeFlags & 8772) !== 0, e = e.child; e !== null;){
                var s = e.alternate, o = t, c = e, g = c.flags;
                switch(c.tag){
                    case 0:
                    case 11:
                    case 15:
                        An(o, c, i), fl(4, c);
                        break;
                    case 1:
                        if (An(o, c, i), s = c, o = s.stateNode, typeof o.componentDidMount == "function") try {
                            o.componentDidMount();
                        } catch (O) {
                            Ct(s, s.return, O);
                        }
                        if (s = c, o = s.updateQueue, o !== null) {
                            var b = s.stateNode;
                            try {
                                var C = o.shared.hiddenCallbacks;
                                if (C !== null) for(o.shared.hiddenCallbacks = null, o = 0; o < C.length; o++)Gh(C[o], b);
                            } catch (O) {
                                Ct(s, s.return, O);
                            }
                        }
                        i && g & 64 && Jm(c), dl(c, c.return);
                        break;
                    case 27:
                        ep(c);
                    case 26:
                    case 5:
                        An(o, c, i), i && s === null && g & 4 && Im(c), dl(c, c.return);
                        break;
                    case 12:
                        An(o, c, i);
                        break;
                    case 31:
                        An(o, c, i), i && g & 4 && sp(o, c);
                        break;
                    case 13:
                        An(o, c, i), i && g & 4 && rp(o, c);
                        break;
                    case 22:
                        c.memoizedState === null && An(o, c, i), dl(c, c.return);
                        break;
                    case 30:
                        break;
                    default:
                        An(o, c, i);
                }
                e = e.sibling;
            }
        }
        function Gu(t, e) {
            var i = null;
            t !== null && t.memoizedState !== null && t.memoizedState.cachePool !== null && (i = t.memoizedState.cachePool.pool), t = null, e.memoizedState !== null && e.memoizedState.cachePool !== null && (t = e.memoizedState.cachePool.pool), t !== i && (t != null && t.refCount++, i != null && Wi(i));
        }
        function qu(t, e) {
            t = null, e.alternate !== null && (t = e.alternate.memoizedState.cache), e = e.memoizedState.cache, e !== t && (e.refCount++, t != null && Wi(t));
        }
        function $e(t, e, i, s) {
            if (e.subtreeFlags & 10256) for(e = e.child; e !== null;)cp(t, e, i, s), e = e.sibling;
        }
        function cp(t, e, i, s) {
            var o = e.flags;
            switch(e.tag){
                case 0:
                case 11:
                case 15:
                    $e(t, e, i, s), o & 2048 && fl(9, e);
                    break;
                case 1:
                    $e(t, e, i, s);
                    break;
                case 3:
                    $e(t, e, i, s), o & 2048 && (t = null, e.alternate !== null && (t = e.alternate.memoizedState.cache), e = e.memoizedState.cache, e !== t && (e.refCount++, t != null && Wi(t)));
                    break;
                case 12:
                    if (o & 2048) {
                        $e(t, e, i, s), t = e.stateNode;
                        try {
                            var c = e.memoizedProps, g = c.id, b = c.onPostCommit;
                            typeof b == "function" && b(g, e.alternate === null ? "mount" : "update", t.passiveEffectDuration, -0);
                        } catch (C) {
                            Ct(e, e.return, C);
                        }
                    } else $e(t, e, i, s);
                    break;
                case 31:
                    $e(t, e, i, s);
                    break;
                case 13:
                    $e(t, e, i, s);
                    break;
                case 23:
                    break;
                case 22:
                    c = e.stateNode, g = e.alternate, e.memoizedState !== null ? c._visibility & 2 ? $e(t, e, i, s) : hl(t, e) : c._visibility & 2 ? $e(t, e, i, s) : (c._visibility |= 2, oi(t, e, i, s, (e.subtreeFlags & 10256) !== 0 || !1)), o & 2048 && Gu(g, e);
                    break;
                case 24:
                    $e(t, e, i, s), o & 2048 && qu(e.alternate, e);
                    break;
                default:
                    $e(t, e, i, s);
            }
        }
        function oi(t, e, i, s, o) {
            for(o = o && ((e.subtreeFlags & 10256) !== 0 || !1), e = e.child; e !== null;){
                var c = t, g = e, b = i, C = s, O = g.flags;
                switch(g.tag){
                    case 0:
                    case 11:
                    case 15:
                        oi(c, g, b, C, o), fl(8, g);
                        break;
                    case 23:
                        break;
                    case 22:
                        var q = g.stateNode;
                        g.memoizedState !== null ? q._visibility & 2 ? oi(c, g, b, C, o) : hl(c, g) : (q._visibility |= 2, oi(c, g, b, C, o)), o && O & 2048 && Gu(g.alternate, g);
                        break;
                    case 24:
                        oi(c, g, b, C, o), o && O & 2048 && qu(g.alternate, g);
                        break;
                    default:
                        oi(c, g, b, C, o);
                }
                e = e.sibling;
            }
        }
        function hl(t, e) {
            if (e.subtreeFlags & 10256) for(e = e.child; e !== null;){
                var i = t, s = e, o = s.flags;
                switch(s.tag){
                    case 22:
                        hl(i, s), o & 2048 && Gu(s.alternate, s);
                        break;
                    case 24:
                        hl(i, s), o & 2048 && qu(s.alternate, s);
                        break;
                    default:
                        hl(i, s);
                }
                e = e.sibling;
            }
        }
        var ml = 8192;
        function ui(t, e, i) {
            if (t.subtreeFlags & ml) for(t = t.child; t !== null;)fp(t, e, i), t = t.sibling;
        }
        function fp(t, e, i) {
            switch(t.tag){
                case 26:
                    ui(t, e, i), t.flags & ml && t.memoizedState !== null && t1(i, Fe, t.memoizedState, t.memoizedProps);
                    break;
                case 5:
                    ui(t, e, i);
                    break;
                case 3:
                case 4:
                    var s = Fe;
                    Fe = or(t.stateNode.containerInfo), ui(t, e, i), Fe = s;
                    break;
                case 22:
                    t.memoizedState === null && (s = t.alternate, s !== null && s.memoizedState !== null ? (s = ml, ml = 16777216, ui(t, e, i), ml = s) : ui(t, e, i));
                    break;
                default:
                    ui(t, e, i);
            }
        }
        function dp(t) {
            var e = t.alternate;
            if (e !== null && (t = e.child, t !== null)) {
                e.child = null;
                do e = t.sibling, t.sibling = null, t = e;
                while (t !== null);
            }
        }
        function pl(t) {
            var e = t.deletions;
            if ((t.flags & 16) !== 0) {
                if (e !== null) for(var i = 0; i < e.length; i++){
                    var s = e[i];
                    ae = s, mp(s, t);
                }
                dp(t);
            }
            if (t.subtreeFlags & 10256) for(t = t.child; t !== null;)hp(t), t = t.sibling;
        }
        function hp(t) {
            switch(t.tag){
                case 0:
                case 11:
                case 15:
                    pl(t), t.flags & 2048 && Pn(9, t, t.return);
                    break;
                case 3:
                    pl(t);
                    break;
                case 12:
                    pl(t);
                    break;
                case 22:
                    var e = t.stateNode;
                    t.memoizedState !== null && e._visibility & 2 && (t.return === null || t.return.tag !== 13) ? (e._visibility &= -3, Fs(t)) : pl(t);
                    break;
                default:
                    pl(t);
            }
        }
        function Fs(t) {
            var e = t.deletions;
            if ((t.flags & 16) !== 0) {
                if (e !== null) for(var i = 0; i < e.length; i++){
                    var s = e[i];
                    ae = s, mp(s, t);
                }
                dp(t);
            }
            for(t = t.child; t !== null;){
                switch(e = t, e.tag){
                    case 0:
                    case 11:
                    case 15:
                        Pn(8, e, e.return), Fs(e);
                        break;
                    case 22:
                        i = e.stateNode, i._visibility & 2 && (i._visibility &= -3, Fs(e));
                        break;
                    default:
                        Fs(e);
                }
                t = t.sibling;
            }
        }
        function mp(t, e) {
            for(; ae !== null;){
                var i = ae;
                switch(i.tag){
                    case 0:
                    case 11:
                    case 15:
                        Pn(8, i, e);
                        break;
                    case 23:
                    case 22:
                        if (i.memoizedState !== null && i.memoizedState.cachePool !== null) {
                            var s = i.memoizedState.cachePool.pool;
                            s != null && s.refCount++;
                        }
                        break;
                    case 24:
                        Wi(i.memoizedState.cache);
                }
                if (s = i.child, s !== null) s.return = i, ae = s;
                else t: for(i = t; ae !== null;){
                    s = ae;
                    var o = s.sibling, c = s.return;
                    if (ip(s), s === i) {
                        ae = null;
                        break t;
                    }
                    if (o !== null) {
                        o.return = c, ae = o;
                        break t;
                    }
                    ae = c;
                }
            }
        }
        var pS = {
            getCacheForType: function(t) {
                var e = re(Zt), i = e.data.get(t);
                return i === void 0 && (i = t(), e.data.set(t, i)), i;
            },
            cacheSignal: function() {
                return re(Zt).controller.signal;
            }
        }, gS = typeof WeakMap == "function" ? WeakMap : Map, Et = 0, jt = null, dt = null, mt = 0, At = 0, je = null, Zn = !1, ci = !1, ku = !1, Cn = 0, qt = 0, Qn = 0, Ma = 0, Yu = 0, Oe = 0, fi = 0, gl = null, Te = null, Xu = !1, $s = 0, pp = 0, Js = 1 / 0, Ws = null, Fn = null, te = 0, $n = null, di = null, wn = 0, Ku = 0, Pu = null, gp = null, yl = 0, Zu = null;
        function Ne() {
            return (Et & 2) !== 0 && mt !== 0 ? mt & -mt : G.T !== null ? Iu() : Od();
        }
        function yp() {
            if (Oe === 0) if ((mt & 536870912) === 0 || vt) {
                var t = ss;
                ss <<= 1, (ss & 3932160) === 0 && (ss = 262144), Oe = t;
            } else Oe = 536870912;
            return t = Me.current, t !== null && (t.flags |= 32), Oe;
        }
        function Ee(t, e, i) {
            (t === jt && (At === 2 || At === 9) || t.cancelPendingCommit !== null) && (hi(t, 0), Jn(t, mt, Oe, !1)), Ui(t, i), ((Et & 2) === 0 || t !== jt) && (t === jt && ((Et & 2) === 0 && (Ma |= i), qt === 4 && Jn(t, mt, Oe, !1)), an(t));
        }
        function vp(t, e, i) {
            if ((Et & 6) !== 0) throw Error(r(327));
            var s = !i && (e & 127) === 0 && (e & t.expiredLanes) === 0 || Bi(t, e), o = s ? bS(t, e) : Fu(t, e, !0), c = s;
            do {
                if (o === 0) {
                    ci && !s && Jn(t, e, 0, !1);
                    break;
                } else {
                    if (i = t.current.alternate, c && !yS(i)) {
                        o = Fu(t, e, !1), c = !1;
                        continue;
                    }
                    if (o === 2) {
                        if (c = e, t.errorRecoveryDisabledLanes & c) var g = 0;
                        else g = t.pendingLanes & -536870913, g = g !== 0 ? g : g & 536870912 ? 536870912 : 0;
                        if (g !== 0) {
                            e = g;
                            t: {
                                var b = t;
                                o = gl;
                                var C = b.current.memoizedState.isDehydrated;
                                if (C && (hi(b, g).flags |= 256), g = Fu(b, g, !1), g !== 2) {
                                    if (ku && !C) {
                                        b.errorRecoveryDisabledLanes |= c, Ma |= c, o = 4;
                                        break t;
                                    }
                                    c = Te, Te = o, c !== null && (Te === null ? Te = c : Te.push.apply(Te, c));
                                }
                                o = g;
                            }
                            if (c = !1, o !== 2) continue;
                        }
                    }
                    if (o === 1) {
                        hi(t, 0), Jn(t, e, 0, !0);
                        break;
                    }
                    t: {
                        switch(s = t, c = o, c){
                            case 0:
                            case 1:
                                throw Error(r(345));
                            case 4:
                                if ((e & 4194048) !== e) break;
                            case 6:
                                Jn(s, e, Oe, !Zn);
                                break t;
                            case 2:
                                Te = null;
                                break;
                            case 3:
                            case 5:
                                break;
                            default:
                                throw Error(r(329));
                        }
                        if ((e & 62914560) === e && (o = $s + 300 - Ae(), 10 < o)) {
                            if (Jn(s, e, Oe, !Zn), os(s, 0, !0) !== 0) break t;
                            wn = e, s.timeoutHandle = Fp(bp.bind(null, s, i, Te, Ws, Xu, e, Oe, Ma, fi, Zn, c, "Throttled", -0, 0), o);
                            break t;
                        }
                        bp(s, i, Te, Ws, Xu, e, Oe, Ma, fi, Zn, c, null, -0, 0);
                    }
                }
                break;
            }while (!0);
            an(t);
        }
        function bp(t, e, i, s, o, c, g, b, C, O, q, K, N, L) {
            if (t.timeoutHandle = -1, K = e.subtreeFlags, K & 8192 || (K & 16785408) === 16785408) {
                K = {
                    stylesheets: null,
                    count: 0,
                    imgCount: 0,
                    imgBytes: 0,
                    suspenseyImages: [],
                    waitingForImages: !0,
                    waitingForViewTransition: !1,
                    unsuspend: fn
                }, fp(e, c, K);
                var I = (c & 62914560) === c ? $s - Ae() : (c & 4194048) === c ? pp - Ae() : 0;
                if (I = e1(K, I), I !== null) {
                    wn = c, t.cancelPendingCommit = I(_p.bind(null, t, e, c, i, s, o, g, b, C, q, K, null, N, L)), Jn(t, c, g, !O);
                    return;
                }
            }
            _p(t, e, c, i, s, o, g, b, C);
        }
        function yS(t) {
            for(var e = t;;){
                var i = e.tag;
                if ((i === 0 || i === 11 || i === 15) && e.flags & 16384 && (i = e.updateQueue, i !== null && (i = i.stores, i !== null))) for(var s = 0; s < i.length; s++){
                    var o = i[s], c = o.getSnapshot;
                    o = o.value;
                    try {
                        if (!_e(c(), o)) return !1;
                    } catch  {
                        return !1;
                    }
                }
                if (i = e.child, e.subtreeFlags & 16384 && i !== null) i.return = e, e = i;
                else {
                    if (e === t) break;
                    for(; e.sibling === null;){
                        if (e.return === null || e.return === t) return !0;
                        e = e.return;
                    }
                    e.sibling.return = e.return, e = e.sibling;
                }
            }
            return !0;
        }
        function Jn(t, e, i, s) {
            e &= ~Yu, e &= ~Ma, t.suspendedLanes |= e, t.pingedLanes &= ~e, s && (t.warmLanes |= e), s = t.expirationTimes;
            for(var o = e; 0 < o;){
                var c = 31 - we(o), g = 1 << c;
                s[c] = -1, o &= ~g;
            }
            i !== 0 && Md(t, i, e);
        }
        function Is() {
            return (Et & 6) === 0 ? (vl(0), !1) : !0;
        }
        function Qu() {
            if (dt !== null) {
                if (At === 0) var t = dt.return;
                else t = dt, pn = xa = null, cu(t), ai = null, tl = 0, t = dt;
                for(; t !== null;)$m(t.alternate, t), t = t.return;
                dt = null;
            }
        }
        function hi(t, e) {
            var i = t.timeoutHandle;
            i !== -1 && (t.timeoutHandle = -1, BS(i)), i = t.cancelPendingCommit, i !== null && (t.cancelPendingCommit = null, i()), wn = 0, Qu(), jt = t, dt = i = hn(t.current, null), mt = e, At = 0, je = null, Zn = !1, ci = Bi(t, e), ku = !1, fi = Oe = Yu = Ma = Qn = qt = 0, Te = gl = null, Xu = !1, (e & 8) !== 0 && (e |= e & 32);
            var s = t.entangledLanes;
            if (s !== 0) for(t = t.entanglements, s &= e; 0 < s;){
                var o = 31 - we(s), c = 1 << o;
                e |= t[o], s &= ~c;
            }
            return Cn = e, xs(), i;
        }
        function xp(t, e) {
            ut = null, G.H = ol, e === ni || e === Rs ? (e = Vh(), At = 3) : e === Wo ? (e = Vh(), At = 4) : At = e === wu ? 8 : e !== null && typeof e == "object" && typeof e.then == "function" ? 6 : 1, je = e, dt === null && (qt = 1, ks(t, Be(e, t.current)));
        }
        function Sp() {
            var t = Me.current;
            return t === null ? !0 : (mt & 4194048) === mt ? qe === null : (mt & 62914560) === mt || (mt & 536870912) !== 0 ? t === qe : !1;
        }
        function Tp() {
            var t = G.H;
            return G.H = ol, t === null ? ol : t;
        }
        function Ep() {
            var t = G.A;
            return G.A = pS, t;
        }
        function tr() {
            qt = 4, Zn || (mt & 4194048) !== mt && Me.current !== null || (ci = !0), (Qn & 134217727) === 0 && (Ma & 134217727) === 0 || jt === null || Jn(jt, mt, Oe, !1);
        }
        function Fu(t, e, i) {
            var s = Et;
            Et |= 2;
            var o = Tp(), c = Ep();
            (jt !== t || mt !== e) && (Ws = null, hi(t, e)), e = !1;
            var g = qt;
            t: do try {
                if (At !== 0 && dt !== null) {
                    var b = dt, C = je;
                    switch(At){
                        case 8:
                            Qu(), g = 6;
                            break t;
                        case 3:
                        case 2:
                        case 9:
                        case 6:
                            Me.current === null && (e = !0);
                            var O = At;
                            if (At = 0, je = null, mi(t, b, C, O), i && ci) {
                                g = 0;
                                break t;
                            }
                            break;
                        default:
                            O = At, At = 0, je = null, mi(t, b, C, O);
                    }
                }
                vS(), g = qt;
                break;
            } catch (q) {
                xp(t, q);
            }
            while (!0);
            return e && t.shellSuspendCounter++, pn = xa = null, Et = s, G.H = o, G.A = c, dt === null && (jt = null, mt = 0, xs()), g;
        }
        function vS() {
            for(; dt !== null;)Ap(dt);
        }
        function bS(t, e) {
            var i = Et;
            Et |= 2;
            var s = Tp(), o = Ep();
            jt !== t || mt !== e ? (Ws = null, Js = Ae() + 500, hi(t, e)) : ci = Bi(t, e);
            t: do try {
                if (At !== 0 && dt !== null) {
                    e = dt;
                    var c = je;
                    e: switch(At){
                        case 1:
                            At = 0, je = null, mi(t, e, c, 1);
                            break;
                        case 2:
                        case 9:
                            if (zh(c)) {
                                At = 0, je = null, Cp(e);
                                break;
                            }
                            e = function() {
                                At !== 2 && At !== 9 || jt !== t || (At = 7), an(t);
                            }, c.then(e, e);
                            break t;
                        case 3:
                            At = 7;
                            break t;
                        case 4:
                            At = 5;
                            break t;
                        case 7:
                            zh(c) ? (At = 0, je = null, Cp(e)) : (At = 0, je = null, mi(t, e, c, 7));
                            break;
                        case 5:
                            var g = null;
                            switch(dt.tag){
                                case 26:
                                    g = dt.memoizedState;
                                case 5:
                                case 27:
                                    var b = dt;
                                    if (g ? cg(g) : b.stateNode.complete) {
                                        At = 0, je = null;
                                        var C = b.sibling;
                                        if (C !== null) dt = C;
                                        else {
                                            var O = b.return;
                                            O !== null ? (dt = O, er(O)) : dt = null;
                                        }
                                        break e;
                                    }
                            }
                            At = 0, je = null, mi(t, e, c, 5);
                            break;
                        case 6:
                            At = 0, je = null, mi(t, e, c, 6);
                            break;
                        case 8:
                            Qu(), qt = 6;
                            break t;
                        default:
                            throw Error(r(462));
                    }
                }
                xS();
                break;
            } catch (q) {
                xp(t, q);
            }
            while (!0);
            return pn = xa = null, G.H = s, G.A = o, Et = i, dt !== null ? 0 : (jt = null, mt = 0, xs(), qt);
        }
        function xS() {
            for(; dt !== null && !Yb();)Ap(dt);
        }
        function Ap(t) {
            var e = Qm(t.alternate, t, Cn);
            t.memoizedProps = t.pendingProps, e === null ? er(t) : dt = e;
        }
        function Cp(t) {
            var e = t, i = e.alternate;
            switch(e.tag){
                case 15:
                case 0:
                    e = km(i, e, e.pendingProps, e.type, void 0, mt);
                    break;
                case 11:
                    e = km(i, e, e.pendingProps, e.type.render, e.ref, mt);
                    break;
                case 5:
                    cu(e);
                default:
                    $m(i, e), e = dt = Eh(e, Cn), e = Qm(i, e, Cn);
            }
            t.memoizedProps = t.pendingProps, e === null ? er(t) : dt = e;
        }
        function mi(t, e, i, s) {
            pn = xa = null, cu(e), ai = null, tl = 0;
            var o = e.return;
            try {
                if (oS(t, o, e, i, mt)) {
                    qt = 1, ks(t, Be(i, t.current)), dt = null;
                    return;
                }
            } catch (c) {
                if (o !== null) throw dt = o, c;
                qt = 1, ks(t, Be(i, t.current)), dt = null;
                return;
            }
            e.flags & 32768 ? (vt || s === 1 ? t = !0 : ci || (mt & 536870912) !== 0 ? t = !1 : (Zn = t = !0, (s === 2 || s === 9 || s === 3 || s === 6) && (s = Me.current, s !== null && s.tag === 13 && (s.flags |= 16384))), wp(e, t)) : er(e);
        }
        function er(t) {
            var e = t;
            do {
                if ((e.flags & 32768) !== 0) {
                    wp(e, Zn);
                    return;
                }
                t = e.return;
                var i = fS(e.alternate, e, Cn);
                if (i !== null) {
                    dt = i;
                    return;
                }
                if (e = e.sibling, e !== null) {
                    dt = e;
                    return;
                }
                dt = e = t;
            }while (e !== null);
            qt === 0 && (qt = 5);
        }
        function wp(t, e) {
            do {
                var i = dS(t.alternate, t);
                if (i !== null) {
                    i.flags &= 32767, dt = i;
                    return;
                }
                if (i = t.return, i !== null && (i.flags |= 32768, i.subtreeFlags = 0, i.deletions = null), !e && (t = t.sibling, t !== null)) {
                    dt = t;
                    return;
                }
                dt = t = i;
            }while (t !== null);
            qt = 6, dt = null;
        }
        function _p(t, e, i, s, o, c, g, b, C) {
            t.cancelPendingCommit = null;
            do nr();
            while (te !== 0);
            if ((Et & 6) !== 0) throw Error(r(327));
            if (e !== null) {
                if (e === t.current) throw Error(r(177));
                if (c = e.lanes | e.childLanes, c |= Bo, Ib(t, i, c, g, b, C), t === jt && (dt = jt = null, mt = 0), di = e, $n = t, wn = i, Ku = c, Pu = o, gp = s, (e.subtreeFlags & 10256) !== 0 || (e.flags & 10256) !== 0 ? (t.callbackNode = null, t.callbackPriority = 0, AS(is, function() {
                    return Op(), null;
                })) : (t.callbackNode = null, t.callbackPriority = 0), s = (e.flags & 13878) !== 0, (e.subtreeFlags & 13878) !== 0 || s) {
                    s = G.T, G.T = null, o = F.p, F.p = 2, g = Et, Et |= 4;
                    try {
                        hS(t, e, i);
                    } finally{
                        Et = g, F.p = o, G.T = s;
                    }
                }
                te = 1, Rp(), Mp(), Dp();
            }
        }
        function Rp() {
            if (te === 1) {
                te = 0;
                var t = $n, e = di, i = (e.flags & 13878) !== 0;
                if ((e.subtreeFlags & 13878) !== 0 || i) {
                    i = G.T, G.T = null;
                    var s = F.p;
                    F.p = 2;
                    var o = Et;
                    Et |= 4;
                    try {
                        op(e, t);
                        var c = rc, g = mh(t.containerInfo), b = c.focusedElem, C = c.selectionRange;
                        if (g !== b && b && b.ownerDocument && hh(b.ownerDocument.documentElement, b)) {
                            if (C !== null && Oo(b)) {
                                var O = C.start, q = C.end;
                                if (q === void 0 && (q = O), "selectionStart" in b) b.selectionStart = O, b.selectionEnd = Math.min(q, b.value.length);
                                else {
                                    var K = b.ownerDocument || document, N = K && K.defaultView || window;
                                    if (N.getSelection) {
                                        var L = N.getSelection(), I = b.textContent.length, at = Math.min(C.start, I), Mt = C.end === void 0 ? at : Math.min(C.end, I);
                                        !L.extend && at > Mt && (g = Mt, Mt = at, at = g);
                                        var D = dh(b, at), _ = dh(b, Mt);
                                        if (D && _ && (L.rangeCount !== 1 || L.anchorNode !== D.node || L.anchorOffset !== D.offset || L.focusNode !== _.node || L.focusOffset !== _.offset)) {
                                            var j = K.createRange();
                                            j.setStart(D.node, D.offset), L.removeAllRanges(), at > Mt ? (L.addRange(j), L.extend(_.node, _.offset)) : (j.setEnd(_.node, _.offset), L.addRange(j));
                                        }
                                    }
                                }
                            }
                            for(K = [], L = b; L = L.parentNode;)L.nodeType === 1 && K.push({
                                element: L,
                                left: L.scrollLeft,
                                top: L.scrollTop
                            });
                            for(typeof b.focus == "function" && b.focus(), b = 0; b < K.length; b++){
                                var k = K[b];
                                k.element.scrollLeft = k.left, k.element.scrollTop = k.top;
                            }
                        }
                        mr = !!sc, rc = sc = null;
                    } finally{
                        Et = o, F.p = s, G.T = i;
                    }
                }
                t.current = e, te = 2;
            }
        }
        function Mp() {
            if (te === 2) {
                te = 0;
                var t = $n, e = di, i = (e.flags & 8772) !== 0;
                if ((e.subtreeFlags & 8772) !== 0 || i) {
                    i = G.T, G.T = null;
                    var s = F.p;
                    F.p = 2;
                    var o = Et;
                    Et |= 4;
                    try {
                        ap(t, e.alternate, e);
                    } finally{
                        Et = o, F.p = s, G.T = i;
                    }
                }
                te = 3;
            }
        }
        function Dp() {
            if (te === 4 || te === 3) {
                te = 0, Xb();
                var t = $n, e = di, i = wn, s = gp;
                (e.subtreeFlags & 10256) !== 0 || (e.flags & 10256) !== 0 ? te = 5 : (te = 0, di = $n = null, jp(t, t.pendingLanes));
                var o = t.pendingLanes;
                if (o === 0 && (Fn = null), ho(i), e = e.stateNode, Ce && typeof Ce.onCommitFiberRoot == "function") try {
                    Ce.onCommitFiberRoot(Vi, e, void 0, (e.current.flags & 128) === 128);
                } catch  {}
                if (s !== null) {
                    e = G.T, o = F.p, F.p = 2, G.T = null;
                    try {
                        for(var c = t.onRecoverableError, g = 0; g < s.length; g++){
                            var b = s[g];
                            c(b.value, {
                                componentStack: b.stack
                            });
                        }
                    } finally{
                        G.T = e, F.p = o;
                    }
                }
                (wn & 3) !== 0 && nr(), an(t), o = t.pendingLanes, (i & 261930) !== 0 && (o & 42) !== 0 ? t === Zu ? yl++ : (yl = 0, Zu = t) : yl = 0, vl(0);
            }
        }
        function jp(t, e) {
            (t.pooledCacheLanes &= e) === 0 && (e = t.pooledCache, e != null && (t.pooledCache = null, Wi(e)));
        }
        function nr() {
            return Rp(), Mp(), Dp(), Op();
        }
        function Op() {
            if (te !== 5) return !1;
            var t = $n, e = Ku;
            Ku = 0;
            var i = ho(wn), s = G.T, o = F.p;
            try {
                F.p = 32 > i ? 32 : i, G.T = null, i = Pu, Pu = null;
                var c = $n, g = wn;
                if (te = 0, di = $n = null, wn = 0, (Et & 6) !== 0) throw Error(r(331));
                var b = Et;
                if (Et |= 4, hp(c.current), cp(c, c.current, g, i), Et = b, vl(0, !1), Ce && typeof Ce.onPostCommitFiberRoot == "function") try {
                    Ce.onPostCommitFiberRoot(Vi, c);
                } catch  {}
                return !0;
            } finally{
                F.p = o, G.T = s, jp(t, e);
            }
        }
        function Np(t, e, i) {
            e = Be(i, e), e = Cu(t.stateNode, e, 2), t = Yn(t, e, 2), t !== null && (Ui(t, 2), an(t));
        }
        function Ct(t, e, i) {
            if (t.tag === 3) Np(t, t, i);
            else for(; e !== null;){
                if (e.tag === 3) {
                    Np(e, t, i);
                    break;
                } else if (e.tag === 1) {
                    var s = e.stateNode;
                    if (typeof e.type.getDerivedStateFromError == "function" || typeof s.componentDidCatch == "function" && (Fn === null || !Fn.has(s))) {
                        t = Be(i, t), i = zm(2), s = Yn(e, i, 2), s !== null && (Lm(i, s, e, t), Ui(s, 2), an(s));
                        break;
                    }
                }
                e = e.return;
            }
        }
        function $u(t, e, i) {
            var s = t.pingCache;
            if (s === null) {
                s = t.pingCache = new gS;
                var o = new Set;
                s.set(e, o);
            } else o = s.get(e), o === void 0 && (o = new Set, s.set(e, o));
            o.has(i) || (ku = !0, o.add(i), t = SS.bind(null, t, e, i), e.then(t, t));
        }
        function SS(t, e, i) {
            var s = t.pingCache;
            s !== null && s.delete(e), t.pingedLanes |= t.suspendedLanes & i, t.warmLanes &= ~i, jt === t && (mt & i) === i && (qt === 4 || qt === 3 && (mt & 62914560) === mt && 300 > Ae() - $s ? (Et & 2) === 0 && hi(t, 0) : Yu |= i, fi === mt && (fi = 0)), an(t);
        }
        function zp(t, e) {
            e === 0 && (e = Rd()), t = ya(t, e), t !== null && (Ui(t, e), an(t));
        }
        function TS(t) {
            var e = t.memoizedState, i = 0;
            e !== null && (i = e.retryLane), zp(t, i);
        }
        function ES(t, e) {
            var i = 0;
            switch(t.tag){
                case 31:
                case 13:
                    var s = t.stateNode, o = t.memoizedState;
                    o !== null && (i = o.retryLane);
                    break;
                case 19:
                    s = t.stateNode;
                    break;
                case 22:
                    s = t.stateNode._retryCache;
                    break;
                default:
                    throw Error(r(314));
            }
            s !== null && s.delete(e), zp(t, i);
        }
        function AS(t, e) {
            return oo(t, e);
        }
        var ar = null, pi = null, Ju = !1, ir = !1, Wu = !1, Wn = 0;
        function an(t) {
            t !== pi && t.next === null && (pi === null ? ar = pi = t : pi = pi.next = t), ir = !0, Ju || (Ju = !0, wS());
        }
        function vl(t, e) {
            if (!Wu && ir) {
                Wu = !0;
                do for(var i = !1, s = ar; s !== null;){
                    if (t !== 0) {
                        var o = s.pendingLanes;
                        if (o === 0) var c = 0;
                        else {
                            var g = s.suspendedLanes, b = s.pingedLanes;
                            c = (1 << 31 - we(42 | t) + 1) - 1, c &= o & ~(g & ~b), c = c & 201326741 ? c & 201326741 | 1 : c ? c | 2 : 0;
                        }
                        c !== 0 && (i = !0, Up(s, c));
                    } else c = mt, c = os(s, s === jt ? c : 0, s.cancelPendingCommit !== null || s.timeoutHandle !== -1), (c & 3) === 0 || Bi(s, c) || (i = !0, Up(s, c));
                    s = s.next;
                }
                while (i);
                Wu = !1;
            }
        }
        function CS() {
            Lp();
        }
        function Lp() {
            ir = Ju = !1;
            var t = 0;
            Wn !== 0 && VS() && (t = Wn);
            for(var e = Ae(), i = null, s = ar; s !== null;){
                var o = s.next, c = Vp(s, e);
                c === 0 ? (s.next = null, i === null ? ar = o : i.next = o, o === null && (pi = i)) : (i = s, (t !== 0 || (c & 3) !== 0) && (ir = !0)), s = o;
            }
            te !== 0 && te !== 5 || vl(t), Wn !== 0 && (Wn = 0);
        }
        function Vp(t, e) {
            for(var i = t.suspendedLanes, s = t.pingedLanes, o = t.expirationTimes, c = t.pendingLanes & -62914561; 0 < c;){
                var g = 31 - we(c), b = 1 << g, C = o[g];
                C === -1 ? ((b & i) === 0 || (b & s) !== 0) && (o[g] = Wb(b, e)) : C <= e && (t.expiredLanes |= b), c &= ~b;
            }
            if (e = jt, i = mt, i = os(t, t === e ? i : 0, t.cancelPendingCommit !== null || t.timeoutHandle !== -1), s = t.callbackNode, i === 0 || t === e && (At === 2 || At === 9) || t.cancelPendingCommit !== null) return s !== null && s !== null && uo(s), t.callbackNode = null, t.callbackPriority = 0;
            if ((i & 3) === 0 || Bi(t, i)) {
                if (e = i & -i, e === t.callbackPriority) return e;
                switch(s !== null && uo(s), ho(i)){
                    case 2:
                    case 8:
                        i = wd;
                        break;
                    case 32:
                        i = is;
                        break;
                    case 268435456:
                        i = _d;
                        break;
                    default:
                        i = is;
                }
                return s = Bp.bind(null, t), i = oo(i, s), t.callbackPriority = e, t.callbackNode = i, e;
            }
            return s !== null && s !== null && uo(s), t.callbackPriority = 2, t.callbackNode = null, 2;
        }
        function Bp(t, e) {
            if (te !== 0 && te !== 5) return t.callbackNode = null, t.callbackPriority = 0, null;
            var i = t.callbackNode;
            if (nr() && t.callbackNode !== i) return null;
            var s = mt;
            return s = os(t, t === jt ? s : 0, t.cancelPendingCommit !== null || t.timeoutHandle !== -1), s === 0 ? null : (vp(t, s, e), Vp(t, Ae()), t.callbackNode != null && t.callbackNode === i ? Bp.bind(null, t) : null);
        }
        function Up(t, e) {
            if (nr()) return null;
            vp(t, e, !0);
        }
        function wS() {
            US(function() {
                (Et & 6) !== 0 ? oo(Cd, CS) : Lp();
            });
        }
        function Iu() {
            if (Wn === 0) {
                var t = ti;
                t === 0 && (t = ls, ls <<= 1, (ls & 261888) === 0 && (ls = 256)), Wn = t;
            }
            return Wn;
        }
        function Hp(t) {
            return t == null || typeof t == "symbol" || typeof t == "boolean" ? null : typeof t == "function" ? t : ds("" + t);
        }
        function Gp(t, e) {
            var i = e.ownerDocument.createElement("input");
            return i.name = e.name, i.value = e.value, t.id && i.setAttribute("form", t.id), e.parentNode.insertBefore(i, e), t = new FormData(t), i.parentNode.removeChild(i), t;
        }
        function _S(t, e, i, s, o) {
            if (e === "submit" && i && i.stateNode === o) {
                var c = Hp((o[ye] || null).action), g = s.submitter;
                g && (e = (e = g[ye] || null) ? Hp(e.formAction) : g.getAttribute("formAction"), e !== null && (c = e, g = null));
                var b = new gs("action", "action", null, s, o);
                t.push({
                    event: b,
                    listeners: [
                        {
                            instance: null,
                            listener: function() {
                                if (s.defaultPrevented) {
                                    if (Wn !== 0) {
                                        var C = g ? Gp(o, g) : new FormData(o);
                                        bu(i, {
                                            pending: !0,
                                            data: C,
                                            method: o.method,
                                            action: c
                                        }, null, C);
                                    }
                                } else typeof c == "function" && (b.preventDefault(), C = g ? Gp(o, g) : new FormData(o), bu(i, {
                                    pending: !0,
                                    data: C,
                                    method: o.method,
                                    action: c
                                }, c, C));
                            },
                            currentTarget: o
                        }
                    ]
                });
            }
        }
        for(var tc = 0; tc < Vo.length; tc++){
            var ec = Vo[tc], RS = ec.toLowerCase(), MS = ec[0].toUpperCase() + ec.slice(1);
            Qe(RS, "on" + MS);
        }
        Qe(yh, "onAnimationEnd"), Qe(vh, "onAnimationIteration"), Qe(bh, "onAnimationStart"), Qe("dblclick", "onDoubleClick"), Qe("focusin", "onFocus"), Qe("focusout", "onBlur"), Qe(Kx, "onTransitionRun"), Qe(Px, "onTransitionStart"), Qe(Zx, "onTransitionCancel"), Qe(xh, "onTransitionEnd"), Ga("onMouseEnter", [
            "mouseout",
            "mouseover"
        ]), Ga("onMouseLeave", [
            "mouseout",
            "mouseover"
        ]), Ga("onPointerEnter", [
            "pointerout",
            "pointerover"
        ]), Ga("onPointerLeave", [
            "pointerout",
            "pointerover"
        ]), ha("onChange", "change click focusin focusout input keydown keyup selectionchange".split(" ")), ha("onSelect", "focusout contextmenu dragend focusin keydown keyup mousedown mouseup selectionchange".split(" ")), ha("onBeforeInput", [
            "compositionend",
            "keypress",
            "textInput",
            "paste"
        ]), ha("onCompositionEnd", "compositionend focusout keydown keypress keyup mousedown".split(" ")), ha("onCompositionStart", "compositionstart focusout keydown keypress keyup mousedown".split(" ")), ha("onCompositionUpdate", "compositionupdate focusout keydown keypress keyup mousedown".split(" "));
        var bl = "abort canplay canplaythrough durationchange emptied encrypted ended error loadeddata loadedmetadata loadstart pause play playing progress ratechange resize seeked seeking stalled suspend timeupdate volumechange waiting".split(" "), DS = new Set("beforetoggle cancel close invalid load scroll scrollend toggle".split(" ").concat(bl));
        function qp(t, e) {
            e = (e & 4) !== 0;
            for(var i = 0; i < t.length; i++){
                var s = t[i], o = s.event;
                s = s.listeners;
                t: {
                    var c = void 0;
                    if (e) for(var g = s.length - 1; 0 <= g; g--){
                        var b = s[g], C = b.instance, O = b.currentTarget;
                        if (b = b.listener, C !== c && o.isPropagationStopped()) break t;
                        c = b, o.currentTarget = O;
                        try {
                            c(o);
                        } catch (q) {
                            bs(q);
                        }
                        o.currentTarget = null, c = C;
                    }
                    else for(g = 0; g < s.length; g++){
                        if (b = s[g], C = b.instance, O = b.currentTarget, b = b.listener, C !== c && o.isPropagationStopped()) break t;
                        c = b, o.currentTarget = O;
                        try {
                            c(o);
                        } catch (q) {
                            bs(q);
                        }
                        o.currentTarget = null, c = C;
                    }
                }
            }
        }
        function ht(t, e) {
            var i = e[mo];
            i === void 0 && (i = e[mo] = new Set);
            var s = t + "__bubble";
            i.has(s) || (kp(e, t, 2, !1), i.add(s));
        }
        function nc(t, e, i) {
            var s = 0;
            e && (s |= 4), kp(i, t, s, e);
        }
        var lr = "_reactListening" + Math.random().toString(36).slice(2);
        function ac(t) {
            if (!t[lr]) {
                t[lr] = !0, Ld.forEach(function(i) {
                    i !== "selectionchange" && (DS.has(i) || nc(i, !1, t), nc(i, !0, t));
                });
                var e = t.nodeType === 9 ? t : t.ownerDocument;
                e === null || e[lr] || (e[lr] = !0, nc("selectionchange", !1, e));
            }
        }
        function kp(t, e, i, s) {
            switch(yg(e)){
                case 2:
                    var o = i1;
                    break;
                case 8:
                    o = l1;
                    break;
                default:
                    o = vc;
            }
            i = o.bind(null, e, i, t), o = void 0, !Eo || e !== "touchstart" && e !== "touchmove" && e !== "wheel" || (o = !0), s ? o !== void 0 ? t.addEventListener(e, i, {
                capture: !0,
                passive: o
            }) : t.addEventListener(e, i, !0) : o !== void 0 ? t.addEventListener(e, i, {
                passive: o
            }) : t.addEventListener(e, i, !1);
        }
        function ic(t, e, i, s, o) {
            var c = s;
            if ((e & 1) === 0 && (e & 2) === 0 && s !== null) t: for(;;){
                if (s === null) return;
                var g = s.tag;
                if (g === 3 || g === 4) {
                    var b = s.stateNode.containerInfo;
                    if (b === o) break;
                    if (g === 4) for(g = s.return; g !== null;){
                        var C = g.tag;
                        if ((C === 3 || C === 4) && g.stateNode.containerInfo === o) return;
                        g = g.return;
                    }
                    for(; b !== null;){
                        if (g = Ba(b), g === null) return;
                        if (C = g.tag, C === 5 || C === 6 || C === 26 || C === 27) {
                            s = c = g;
                            continue t;
                        }
                        b = b.parentNode;
                    }
                }
                s = s.return;
            }
            Zd(function() {
                var O = c, q = So(i), K = [];
                t: {
                    var N = Sh.get(t);
                    if (N !== void 0) {
                        var L = gs, I = t;
                        switch(t){
                            case "keypress":
                                if (ms(i) === 0) break t;
                            case "keydown":
                            case "keyup":
                                L = Ex;
                                break;
                            case "focusin":
                                I = "focus", L = _o;
                                break;
                            case "focusout":
                                I = "blur", L = _o;
                                break;
                            case "beforeblur":
                            case "afterblur":
                                L = _o;
                                break;
                            case "click":
                                if (i.button === 2) break t;
                            case "auxclick":
                            case "dblclick":
                            case "mousedown":
                            case "mousemove":
                            case "mouseup":
                            case "mouseout":
                            case "mouseover":
                            case "contextmenu":
                                L = $d;
                                break;
                            case "drag":
                            case "dragend":
                            case "dragenter":
                            case "dragexit":
                            case "dragleave":
                            case "dragover":
                            case "dragstart":
                            case "drop":
                                L = fx;
                                break;
                            case "touchcancel":
                            case "touchend":
                            case "touchmove":
                            case "touchstart":
                                L = wx;
                                break;
                            case yh:
                            case vh:
                            case bh:
                                L = mx;
                                break;
                            case xh:
                                L = Rx;
                                break;
                            case "scroll":
                            case "scrollend":
                                L = ux;
                                break;
                            case "wheel":
                                L = Dx;
                                break;
                            case "copy":
                            case "cut":
                            case "paste":
                                L = gx;
                                break;
                            case "gotpointercapture":
                            case "lostpointercapture":
                            case "pointercancel":
                            case "pointerdown":
                            case "pointermove":
                            case "pointerout":
                            case "pointerover":
                            case "pointerup":
                                L = Wd;
                                break;
                            case "toggle":
                            case "beforetoggle":
                                L = Ox;
                        }
                        var at = (e & 4) !== 0, Mt = !at && (t === "scroll" || t === "scrollend"), D = at ? N !== null ? N + "Capture" : null : N;
                        at = [];
                        for(var _ = O, j; _ !== null;){
                            var k = _;
                            if (j = k.stateNode, k = k.tag, k !== 5 && k !== 26 && k !== 27 || j === null || D === null || (k = qi(_, D), k != null && at.push(xl(_, k, j))), Mt) break;
                            _ = _.return;
                        }
                        0 < at.length && (N = new L(N, I, null, i, q), K.push({
                            event: N,
                            listeners: at
                        }));
                    }
                }
                if ((e & 7) === 0) {
                    t: {
                        if (N = t === "mouseover" || t === "pointerover", L = t === "mouseout" || t === "pointerout", N && i !== xo && (I = i.relatedTarget || i.fromElement) && (Ba(I) || I[Va])) break t;
                        if ((L || N) && (N = q.window === q ? q : (N = q.ownerDocument) ? N.defaultView || N.parentWindow : window, L ? (I = i.relatedTarget || i.toElement, L = O, I = I ? Ba(I) : null, I !== null && (Mt = d(I), at = I.tag, I !== Mt || at !== 5 && at !== 27 && at !== 6) && (I = null)) : (L = null, I = O), L !== I)) {
                            if (at = $d, k = "onMouseLeave", D = "onMouseEnter", _ = "mouse", (t === "pointerout" || t === "pointerover") && (at = Wd, k = "onPointerLeave", D = "onPointerEnter", _ = "pointer"), Mt = L == null ? N : Gi(L), j = I == null ? N : Gi(I), N = new at(k, _ + "leave", L, i, q), N.target = Mt, N.relatedTarget = j, k = null, Ba(q) === O && (at = new at(D, _ + "enter", I, i, q), at.target = j, at.relatedTarget = Mt, k = at), Mt = k, L && I) e: {
                                for(at = jS, D = L, _ = I, j = 0, k = D; k; k = at(k))j++;
                                k = 0;
                                for(var nt = _; nt; nt = at(nt))k++;
                                for(; 0 < j - k;)D = at(D), j--;
                                for(; 0 < k - j;)_ = at(_), k--;
                                for(; j--;){
                                    if (D === _ || _ !== null && D === _.alternate) {
                                        at = D;
                                        break e;
                                    }
                                    D = at(D), _ = at(_);
                                }
                                at = null;
                            }
                            else at = null;
                            L !== null && Yp(K, N, L, at, !1), I !== null && Mt !== null && Yp(K, Mt, I, at, !0);
                        }
                    }
                    t: {
                        if (N = O ? Gi(O) : window, L = N.nodeName && N.nodeName.toLowerCase(), L === "select" || L === "input" && N.type === "file") var xt = sh;
                        else if (ih(N)) if (rh) xt = kx;
                        else {
                            xt = Gx;
                            var et = Hx;
                        }
                        else L = N.nodeName, !L || L.toLowerCase() !== "input" || N.type !== "checkbox" && N.type !== "radio" ? O && bo(O.elementType) && (xt = sh) : xt = qx;
                        if (xt && (xt = xt(t, O))) {
                            lh(K, xt, i, q);
                            break t;
                        }
                        et && et(t, N, O), t === "focusout" && O && N.type === "number" && O.memoizedProps.value != null && vo(N, "number", N.value);
                    }
                    switch(et = O ? Gi(O) : window, t){
                        case "focusin":
                            (ih(et) || et.contentEditable === "true") && (Pa = et, No = O, Fi = null);
                            break;
                        case "focusout":
                            Fi = No = Pa = null;
                            break;
                        case "mousedown":
                            zo = !0;
                            break;
                        case "contextmenu":
                        case "mouseup":
                        case "dragend":
                            zo = !1, ph(K, i, q);
                            break;
                        case "selectionchange":
                            if (Xx) break;
                        case "keydown":
                        case "keyup":
                            ph(K, i, q);
                    }
                    var ct;
                    if (Mo) t: {
                        switch(t){
                            case "compositionstart":
                                var pt = "onCompositionStart";
                                break t;
                            case "compositionend":
                                pt = "onCompositionEnd";
                                break t;
                            case "compositionupdate":
                                pt = "onCompositionUpdate";
                                break t;
                        }
                        pt = void 0;
                    }
                    else Ka ? nh(t, i) && (pt = "onCompositionEnd") : t === "keydown" && i.keyCode === 229 && (pt = "onCompositionStart");
                    pt && (Id && i.locale !== "ko" && (Ka || pt !== "onCompositionStart" ? pt === "onCompositionEnd" && Ka && (ct = Qd()) : (Vn = q, Ao = "value" in Vn ? Vn.value : Vn.textContent, Ka = !0)), et = sr(O, pt), 0 < et.length && (pt = new Jd(pt, t, null, i, q), K.push({
                        event: pt,
                        listeners: et
                    }), ct ? pt.data = ct : (ct = ah(i), ct !== null && (pt.data = ct)))), (ct = zx ? Lx(t, i) : Vx(t, i)) && (pt = sr(O, "onBeforeInput"), 0 < pt.length && (et = new Jd("onBeforeInput", "beforeinput", null, i, q), K.push({
                        event: et,
                        listeners: pt
                    }), et.data = ct)), _S(K, t, O, i, q);
                }
                qp(K, e);
            });
        }
        function xl(t, e, i) {
            return {
                instance: t,
                listener: e,
                currentTarget: i
            };
        }
        function sr(t, e) {
            for(var i = e + "Capture", s = []; t !== null;){
                var o = t, c = o.stateNode;
                if (o = o.tag, o !== 5 && o !== 26 && o !== 27 || c === null || (o = qi(t, i), o != null && s.unshift(xl(t, o, c)), o = qi(t, e), o != null && s.push(xl(t, o, c))), t.tag === 3) return s;
                t = t.return;
            }
            return [];
        }
        function jS(t) {
            if (t === null) return null;
            do t = t.return;
            while (t && t.tag !== 5 && t.tag !== 27);
            return t || null;
        }
        function Yp(t, e, i, s, o) {
            for(var c = e._reactName, g = []; i !== null && i !== s;){
                var b = i, C = b.alternate, O = b.stateNode;
                if (b = b.tag, C !== null && C === s) break;
                b !== 5 && b !== 26 && b !== 27 || O === null || (C = O, o ? (O = qi(i, c), O != null && g.unshift(xl(i, O, C))) : o || (O = qi(i, c), O != null && g.push(xl(i, O, C)))), i = i.return;
            }
            g.length !== 0 && t.push({
                event: e,
                listeners: g
            });
        }
        var OS = /\r\n?/g, NS = /\u0000|\uFFFD/g;
        function Xp(t) {
            return (typeof t == "string" ? t : "" + t).replace(OS, `
`).replace(NS, "");
        }
        function Kp(t, e) {
            return e = Xp(e), Xp(t) === e;
        }
        function Rt(t, e, i, s, o, c) {
            switch(i){
                case "children":
                    typeof s == "string" ? e === "body" || e === "textarea" && s === "" || ka(t, s) : (typeof s == "number" || typeof s == "bigint") && e !== "body" && ka(t, "" + s);
                    break;
                case "className":
                    cs(t, "class", s);
                    break;
                case "tabIndex":
                    cs(t, "tabindex", s);
                    break;
                case "dir":
                case "role":
                case "viewBox":
                case "width":
                case "height":
                    cs(t, i, s);
                    break;
                case "style":
                    Kd(t, s, c);
                    break;
                case "data":
                    if (e !== "object") {
                        cs(t, "data", s);
                        break;
                    }
                case "src":
                case "href":
                    if (s === "" && (e !== "a" || i !== "href")) {
                        t.removeAttribute(i);
                        break;
                    }
                    if (s == null || typeof s == "function" || typeof s == "symbol" || typeof s == "boolean") {
                        t.removeAttribute(i);
                        break;
                    }
                    s = ds("" + s), t.setAttribute(i, s);
                    break;
                case "action":
                case "formAction":
                    if (typeof s == "function") {
                        t.setAttribute(i, "javascript:throw new Error('A React form was unexpectedly submitted. If you called form.submit() manually, consider using form.requestSubmit() instead. If you\\'re trying to use event.stopPropagation() in a submit event handler, consider also calling event.preventDefault().')");
                        break;
                    } else typeof c == "function" && (i === "formAction" ? (e !== "input" && Rt(t, e, "name", o.name, o, null), Rt(t, e, "formEncType", o.formEncType, o, null), Rt(t, e, "formMethod", o.formMethod, o, null), Rt(t, e, "formTarget", o.formTarget, o, null)) : (Rt(t, e, "encType", o.encType, o, null), Rt(t, e, "method", o.method, o, null), Rt(t, e, "target", o.target, o, null)));
                    if (s == null || typeof s == "symbol" || typeof s == "boolean") {
                        t.removeAttribute(i);
                        break;
                    }
                    s = ds("" + s), t.setAttribute(i, s);
                    break;
                case "onClick":
                    s != null && (t.onclick = fn);
                    break;
                case "onScroll":
                    s != null && ht("scroll", t);
                    break;
                case "onScrollEnd":
                    s != null && ht("scrollend", t);
                    break;
                case "dangerouslySetInnerHTML":
                    if (s != null) {
                        if (typeof s != "object" || !("__html" in s)) throw Error(r(61));
                        if (i = s.__html, i != null) {
                            if (o.children != null) throw Error(r(60));
                            t.innerHTML = i;
                        }
                    }
                    break;
                case "multiple":
                    t.multiple = s && typeof s != "function" && typeof s != "symbol";
                    break;
                case "muted":
                    t.muted = s && typeof s != "function" && typeof s != "symbol";
                    break;
                case "suppressContentEditableWarning":
                case "suppressHydrationWarning":
                case "defaultValue":
                case "defaultChecked":
                case "innerHTML":
                case "ref":
                    break;
                case "autoFocus":
                    break;
                case "xlinkHref":
                    if (s == null || typeof s == "function" || typeof s == "boolean" || typeof s == "symbol") {
                        t.removeAttribute("xlink:href");
                        break;
                    }
                    i = ds("" + s), t.setAttributeNS("http://www.w3.org/1999/xlink", "xlink:href", i);
                    break;
                case "contentEditable":
                case "spellCheck":
                case "draggable":
                case "value":
                case "autoReverse":
                case "externalResourcesRequired":
                case "focusable":
                case "preserveAlpha":
                    s != null && typeof s != "function" && typeof s != "symbol" ? t.setAttribute(i, "" + s) : t.removeAttribute(i);
                    break;
                case "inert":
                case "allowFullScreen":
                case "async":
                case "autoPlay":
                case "controls":
                case "default":
                case "defer":
                case "disabled":
                case "disablePictureInPicture":
                case "disableRemotePlayback":
                case "formNoValidate":
                case "hidden":
                case "loop":
                case "noModule":
                case "noValidate":
                case "open":
                case "playsInline":
                case "readOnly":
                case "required":
                case "reversed":
                case "scoped":
                case "seamless":
                case "itemScope":
                    s && typeof s != "function" && typeof s != "symbol" ? t.setAttribute(i, "") : t.removeAttribute(i);
                    break;
                case "capture":
                case "download":
                    s === !0 ? t.setAttribute(i, "") : s !== !1 && s != null && typeof s != "function" && typeof s != "symbol" ? t.setAttribute(i, s) : t.removeAttribute(i);
                    break;
                case "cols":
                case "rows":
                case "size":
                case "span":
                    s != null && typeof s != "function" && typeof s != "symbol" && !isNaN(s) && 1 <= s ? t.setAttribute(i, s) : t.removeAttribute(i);
                    break;
                case "rowSpan":
                case "start":
                    s == null || typeof s == "function" || typeof s == "symbol" || isNaN(s) ? t.removeAttribute(i) : t.setAttribute(i, s);
                    break;
                case "popover":
                    ht("beforetoggle", t), ht("toggle", t), us(t, "popover", s);
                    break;
                case "xlinkActuate":
                    cn(t, "http://www.w3.org/1999/xlink", "xlink:actuate", s);
                    break;
                case "xlinkArcrole":
                    cn(t, "http://www.w3.org/1999/xlink", "xlink:arcrole", s);
                    break;
                case "xlinkRole":
                    cn(t, "http://www.w3.org/1999/xlink", "xlink:role", s);
                    break;
                case "xlinkShow":
                    cn(t, "http://www.w3.org/1999/xlink", "xlink:show", s);
                    break;
                case "xlinkTitle":
                    cn(t, "http://www.w3.org/1999/xlink", "xlink:title", s);
                    break;
                case "xlinkType":
                    cn(t, "http://www.w3.org/1999/xlink", "xlink:type", s);
                    break;
                case "xmlBase":
                    cn(t, "http://www.w3.org/XML/1998/namespace", "xml:base", s);
                    break;
                case "xmlLang":
                    cn(t, "http://www.w3.org/XML/1998/namespace", "xml:lang", s);
                    break;
                case "xmlSpace":
                    cn(t, "http://www.w3.org/XML/1998/namespace", "xml:space", s);
                    break;
                case "is":
                    us(t, "is", s);
                    break;
                case "innerText":
                case "textContent":
                    break;
                default:
                    (!(2 < i.length) || i[0] !== "o" && i[0] !== "O" || i[1] !== "n" && i[1] !== "N") && (i = rx.get(i) || i, us(t, i, s));
            }
        }
        function lc(t, e, i, s, o, c) {
            switch(i){
                case "style":
                    Kd(t, s, c);
                    break;
                case "dangerouslySetInnerHTML":
                    if (s != null) {
                        if (typeof s != "object" || !("__html" in s)) throw Error(r(61));
                        if (i = s.__html, i != null) {
                            if (o.children != null) throw Error(r(60));
                            t.innerHTML = i;
                        }
                    }
                    break;
                case "children":
                    typeof s == "string" ? ka(t, s) : (typeof s == "number" || typeof s == "bigint") && ka(t, "" + s);
                    break;
                case "onScroll":
                    s != null && ht("scroll", t);
                    break;
                case "onScrollEnd":
                    s != null && ht("scrollend", t);
                    break;
                case "onClick":
                    s != null && (t.onclick = fn);
                    break;
                case "suppressContentEditableWarning":
                case "suppressHydrationWarning":
                case "innerHTML":
                case "ref":
                    break;
                case "innerText":
                case "textContent":
                    break;
                default:
                    if (!Vd.hasOwnProperty(i)) t: {
                        if (i[0] === "o" && i[1] === "n" && (o = i.endsWith("Capture"), e = i.slice(2, o ? i.length - 7 : void 0), c = t[ye] || null, c = c != null ? c[i] : null, typeof c == "function" && t.removeEventListener(e, c, o), typeof s == "function")) {
                            typeof c != "function" && c !== null && (i in t ? t[i] = null : t.hasAttribute(i) && t.removeAttribute(i)), t.addEventListener(e, s, o);
                            break t;
                        }
                        i in t ? t[i] = s : s === !0 ? t.setAttribute(i, "") : us(t, i, s);
                    }
            }
        }
        function ue(t, e, i) {
            switch(e){
                case "div":
                case "span":
                case "svg":
                case "path":
                case "a":
                case "g":
                case "p":
                case "li":
                    break;
                case "img":
                    ht("error", t), ht("load", t);
                    var s = !1, o = !1, c;
                    for(c in i)if (i.hasOwnProperty(c)) {
                        var g = i[c];
                        if (g != null) switch(c){
                            case "src":
                                s = !0;
                                break;
                            case "srcSet":
                                o = !0;
                                break;
                            case "children":
                            case "dangerouslySetInnerHTML":
                                throw Error(r(137, e));
                            default:
                                Rt(t, e, c, g, i, null);
                        }
                    }
                    o && Rt(t, e, "srcSet", i.srcSet, i, null), s && Rt(t, e, "src", i.src, i, null);
                    return;
                case "input":
                    ht("invalid", t);
                    var b = c = g = o = null, C = null, O = null;
                    for(s in i)if (i.hasOwnProperty(s)) {
                        var q = i[s];
                        if (q != null) switch(s){
                            case "name":
                                o = q;
                                break;
                            case "type":
                                g = q;
                                break;
                            case "checked":
                                C = q;
                                break;
                            case "defaultChecked":
                                O = q;
                                break;
                            case "value":
                                c = q;
                                break;
                            case "defaultValue":
                                b = q;
                                break;
                            case "children":
                            case "dangerouslySetInnerHTML":
                                if (q != null) throw Error(r(137, e));
                                break;
                            default:
                                Rt(t, e, s, q, i, null);
                        }
                    }
                    qd(t, c, b, C, O, g, o, !1);
                    return;
                case "select":
                    ht("invalid", t), s = g = c = null;
                    for(o in i)if (i.hasOwnProperty(o) && (b = i[o], b != null)) switch(o){
                        case "value":
                            c = b;
                            break;
                        case "defaultValue":
                            g = b;
                            break;
                        case "multiple":
                            s = b;
                        default:
                            Rt(t, e, o, b, i, null);
                    }
                    e = c, i = g, t.multiple = !!s, e != null ? qa(t, !!s, e, !1) : i != null && qa(t, !!s, i, !0);
                    return;
                case "textarea":
                    ht("invalid", t), c = o = s = null;
                    for(g in i)if (i.hasOwnProperty(g) && (b = i[g], b != null)) switch(g){
                        case "value":
                            s = b;
                            break;
                        case "defaultValue":
                            o = b;
                            break;
                        case "children":
                            c = b;
                            break;
                        case "dangerouslySetInnerHTML":
                            if (b != null) throw Error(r(91));
                            break;
                        default:
                            Rt(t, e, g, b, i, null);
                    }
                    Yd(t, s, o, c);
                    return;
                case "option":
                    for(C in i)if (i.hasOwnProperty(C) && (s = i[C], s != null)) switch(C){
                        case "selected":
                            t.selected = s && typeof s != "function" && typeof s != "symbol";
                            break;
                        default:
                            Rt(t, e, C, s, i, null);
                    }
                    return;
                case "dialog":
                    ht("beforetoggle", t), ht("toggle", t), ht("cancel", t), ht("close", t);
                    break;
                case "iframe":
                case "object":
                    ht("load", t);
                    break;
                case "video":
                case "audio":
                    for(s = 0; s < bl.length; s++)ht(bl[s], t);
                    break;
                case "image":
                    ht("error", t), ht("load", t);
                    break;
                case "details":
                    ht("toggle", t);
                    break;
                case "embed":
                case "source":
                case "link":
                    ht("error", t), ht("load", t);
                case "area":
                case "base":
                case "br":
                case "col":
                case "hr":
                case "keygen":
                case "meta":
                case "param":
                case "track":
                case "wbr":
                case "menuitem":
                    for(O in i)if (i.hasOwnProperty(O) && (s = i[O], s != null)) switch(O){
                        case "children":
                        case "dangerouslySetInnerHTML":
                            throw Error(r(137, e));
                        default:
                            Rt(t, e, O, s, i, null);
                    }
                    return;
                default:
                    if (bo(e)) {
                        for(q in i)i.hasOwnProperty(q) && (s = i[q], s !== void 0 && lc(t, e, q, s, i, void 0));
                        return;
                    }
            }
            for(b in i)i.hasOwnProperty(b) && (s = i[b], s != null && Rt(t, e, b, s, i, null));
        }
        function zS(t, e, i, s) {
            switch(e){
                case "div":
                case "span":
                case "svg":
                case "path":
                case "a":
                case "g":
                case "p":
                case "li":
                    break;
                case "input":
                    var o = null, c = null, g = null, b = null, C = null, O = null, q = null;
                    for(L in i){
                        var K = i[L];
                        if (i.hasOwnProperty(L) && K != null) switch(L){
                            case "checked":
                                break;
                            case "value":
                                break;
                            case "defaultValue":
                                C = K;
                            default:
                                s.hasOwnProperty(L) || Rt(t, e, L, null, s, K);
                        }
                    }
                    for(var N in s){
                        var L = s[N];
                        if (K = i[N], s.hasOwnProperty(N) && (L != null || K != null)) switch(N){
                            case "type":
                                c = L;
                                break;
                            case "name":
                                o = L;
                                break;
                            case "checked":
                                O = L;
                                break;
                            case "defaultChecked":
                                q = L;
                                break;
                            case "value":
                                g = L;
                                break;
                            case "defaultValue":
                                b = L;
                                break;
                            case "children":
                            case "dangerouslySetInnerHTML":
                                if (L != null) throw Error(r(137, e));
                                break;
                            default:
                                L !== K && Rt(t, e, N, L, s, K);
                        }
                    }
                    yo(t, g, b, C, O, q, c, o);
                    return;
                case "select":
                    L = g = b = N = null;
                    for(c in i)if (C = i[c], i.hasOwnProperty(c) && C != null) switch(c){
                        case "value":
                            break;
                        case "multiple":
                            L = C;
                        default:
                            s.hasOwnProperty(c) || Rt(t, e, c, null, s, C);
                    }
                    for(o in s)if (c = s[o], C = i[o], s.hasOwnProperty(o) && (c != null || C != null)) switch(o){
                        case "value":
                            N = c;
                            break;
                        case "defaultValue":
                            b = c;
                            break;
                        case "multiple":
                            g = c;
                        default:
                            c !== C && Rt(t, e, o, c, s, C);
                    }
                    e = b, i = g, s = L, N != null ? qa(t, !!i, N, !1) : !!s != !!i && (e != null ? qa(t, !!i, e, !0) : qa(t, !!i, i ? [] : "", !1));
                    return;
                case "textarea":
                    L = N = null;
                    for(b in i)if (o = i[b], i.hasOwnProperty(b) && o != null && !s.hasOwnProperty(b)) switch(b){
                        case "value":
                            break;
                        case "children":
                            break;
                        default:
                            Rt(t, e, b, null, s, o);
                    }
                    for(g in s)if (o = s[g], c = i[g], s.hasOwnProperty(g) && (o != null || c != null)) switch(g){
                        case "value":
                            N = o;
                            break;
                        case "defaultValue":
                            L = o;
                            break;
                        case "children":
                            break;
                        case "dangerouslySetInnerHTML":
                            if (o != null) throw Error(r(91));
                            break;
                        default:
                            o !== c && Rt(t, e, g, o, s, c);
                    }
                    kd(t, N, L);
                    return;
                case "option":
                    for(var I in i)if (N = i[I], i.hasOwnProperty(I) && N != null && !s.hasOwnProperty(I)) switch(I){
                        case "selected":
                            t.selected = !1;
                            break;
                        default:
                            Rt(t, e, I, null, s, N);
                    }
                    for(C in s)if (N = s[C], L = i[C], s.hasOwnProperty(C) && N !== L && (N != null || L != null)) switch(C){
                        case "selected":
                            t.selected = N && typeof N != "function" && typeof N != "symbol";
                            break;
                        default:
                            Rt(t, e, C, N, s, L);
                    }
                    return;
                case "img":
                case "link":
                case "area":
                case "base":
                case "br":
                case "col":
                case "embed":
                case "hr":
                case "keygen":
                case "meta":
                case "param":
                case "source":
                case "track":
                case "wbr":
                case "menuitem":
                    for(var at in i)N = i[at], i.hasOwnProperty(at) && N != null && !s.hasOwnProperty(at) && Rt(t, e, at, null, s, N);
                    for(O in s)if (N = s[O], L = i[O], s.hasOwnProperty(O) && N !== L && (N != null || L != null)) switch(O){
                        case "children":
                        case "dangerouslySetInnerHTML":
                            if (N != null) throw Error(r(137, e));
                            break;
                        default:
                            Rt(t, e, O, N, s, L);
                    }
                    return;
                default:
                    if (bo(e)) {
                        for(var Mt in i)N = i[Mt], i.hasOwnProperty(Mt) && N !== void 0 && !s.hasOwnProperty(Mt) && lc(t, e, Mt, void 0, s, N);
                        for(q in s)N = s[q], L = i[q], !s.hasOwnProperty(q) || N === L || N === void 0 && L === void 0 || lc(t, e, q, N, s, L);
                        return;
                    }
            }
            for(var D in i)N = i[D], i.hasOwnProperty(D) && N != null && !s.hasOwnProperty(D) && Rt(t, e, D, null, s, N);
            for(K in s)N = s[K], L = i[K], !s.hasOwnProperty(K) || N === L || N == null && L == null || Rt(t, e, K, N, s, L);
        }
        function Pp(t) {
            switch(t){
                case "css":
                case "script":
                case "font":
                case "img":
                case "image":
                case "input":
                case "link":
                    return !0;
                default:
                    return !1;
            }
        }
        function LS() {
            if (typeof performance.getEntriesByType == "function") {
                for(var t = 0, e = 0, i = performance.getEntriesByType("resource"), s = 0; s < i.length; s++){
                    var o = i[s], c = o.transferSize, g = o.initiatorType, b = o.duration;
                    if (c && b && Pp(g)) {
                        for(g = 0, b = o.responseEnd, s += 1; s < i.length; s++){
                            var C = i[s], O = C.startTime;
                            if (O > b) break;
                            var q = C.transferSize, K = C.initiatorType;
                            q && Pp(K) && (C = C.responseEnd, g += q * (C < b ? 1 : (b - O) / (C - O)));
                        }
                        if (--s, e += 8 * (c + g) / (o.duration / 1e3), t++, 10 < t) break;
                    }
                }
                if (0 < t) return e / t / 1e6;
            }
            return navigator.connection && (t = navigator.connection.downlink, typeof t == "number") ? t : 5;
        }
        var sc = null, rc = null;
        function rr(t) {
            return t.nodeType === 9 ? t : t.ownerDocument;
        }
        function Zp(t) {
            switch(t){
                case "http://www.w3.org/2000/svg":
                    return 1;
                case "http://www.w3.org/1998/Math/MathML":
                    return 2;
                default:
                    return 0;
            }
        }
        function Qp(t, e) {
            if (t === 0) switch(e){
                case "svg":
                    return 1;
                case "math":
                    return 2;
                default:
                    return 0;
            }
            return t === 1 && e === "foreignObject" ? 0 : t;
        }
        function oc(t, e) {
            return t === "textarea" || t === "noscript" || typeof e.children == "string" || typeof e.children == "number" || typeof e.children == "bigint" || typeof e.dangerouslySetInnerHTML == "object" && e.dangerouslySetInnerHTML !== null && e.dangerouslySetInnerHTML.__html != null;
        }
        var uc = null;
        function VS() {
            var t = window.event;
            return t && t.type === "popstate" ? t === uc ? !1 : (uc = t, !0) : (uc = null, !1);
        }
        var Fp = typeof setTimeout == "function" ? setTimeout : void 0, BS = typeof clearTimeout == "function" ? clearTimeout : void 0, $p = typeof Promise == "function" ? Promise : void 0, US = typeof queueMicrotask == "function" ? queueMicrotask : typeof $p < "u" ? function(t) {
            return $p.resolve(null).then(t).catch(HS);
        } : Fp;
        function HS(t) {
            setTimeout(function() {
                throw t;
            });
        }
        function In(t) {
            return t === "head";
        }
        function Jp(t, e) {
            var i = e, s = 0;
            do {
                var o = i.nextSibling;
                if (t.removeChild(i), o && o.nodeType === 8) if (i = o.data, i === "/$" || i === "/&") {
                    if (s === 0) {
                        t.removeChild(o), bi(e);
                        return;
                    }
                    s--;
                } else if (i === "$" || i === "$?" || i === "$~" || i === "$!" || i === "&") s++;
                else if (i === "html") Sl(t.ownerDocument.documentElement);
                else if (i === "head") {
                    i = t.ownerDocument.head, Sl(i);
                    for(var c = i.firstChild; c;){
                        var g = c.nextSibling, b = c.nodeName;
                        c[Hi] || b === "SCRIPT" || b === "STYLE" || b === "LINK" && c.rel.toLowerCase() === "stylesheet" || i.removeChild(c), c = g;
                    }
                } else i === "body" && Sl(t.ownerDocument.body);
                i = o;
            }while (i);
            bi(e);
        }
        function Wp(t, e) {
            var i = t;
            t = 0;
            do {
                var s = i.nextSibling;
                if (i.nodeType === 1 ? e ? (i._stashedDisplay = i.style.display, i.style.display = "none") : (i.style.display = i._stashedDisplay || "", i.getAttribute("style") === "" && i.removeAttribute("style")) : i.nodeType === 3 && (e ? (i._stashedText = i.nodeValue, i.nodeValue = "") : i.nodeValue = i._stashedText || ""), s && s.nodeType === 8) if (i = s.data, i === "/$") {
                    if (t === 0) break;
                    t--;
                } else i !== "$" && i !== "$?" && i !== "$~" && i !== "$!" || t++;
                i = s;
            }while (i);
        }
        function cc(t) {
            var e = t.firstChild;
            for(e && e.nodeType === 10 && (e = e.nextSibling); e;){
                var i = e;
                switch(e = e.nextSibling, i.nodeName){
                    case "HTML":
                    case "HEAD":
                    case "BODY":
                        cc(i), po(i);
                        continue;
                    case "SCRIPT":
                    case "STYLE":
                        continue;
                    case "LINK":
                        if (i.rel.toLowerCase() === "stylesheet") continue;
                }
                t.removeChild(i);
            }
        }
        function GS(t, e, i, s) {
            for(; t.nodeType === 1;){
                var o = i;
                if (t.nodeName.toLowerCase() !== e.toLowerCase()) {
                    if (!s && (t.nodeName !== "INPUT" || t.type !== "hidden")) break;
                } else if (s) {
                    if (!t[Hi]) switch(e){
                        case "meta":
                            if (!t.hasAttribute("itemprop")) break;
                            return t;
                        case "link":
                            if (c = t.getAttribute("rel"), c === "stylesheet" && t.hasAttribute("data-precedence")) break;
                            if (c !== o.rel || t.getAttribute("href") !== (o.href == null || o.href === "" ? null : o.href) || t.getAttribute("crossorigin") !== (o.crossOrigin == null ? null : o.crossOrigin) || t.getAttribute("title") !== (o.title == null ? null : o.title)) break;
                            return t;
                        case "style":
                            if (t.hasAttribute("data-precedence")) break;
                            return t;
                        case "script":
                            if (c = t.getAttribute("src"), (c !== (o.src == null ? null : o.src) || t.getAttribute("type") !== (o.type == null ? null : o.type) || t.getAttribute("crossorigin") !== (o.crossOrigin == null ? null : o.crossOrigin)) && c && t.hasAttribute("async") && !t.hasAttribute("itemprop")) break;
                            return t;
                        default:
                            return t;
                    }
                } else if (e === "input" && t.type === "hidden") {
                    var c = o.name == null ? null : "" + o.name;
                    if (o.type === "hidden" && t.getAttribute("name") === c) return t;
                } else return t;
                if (t = ke(t.nextSibling), t === null) break;
            }
            return null;
        }
        function qS(t, e, i) {
            if (e === "") return null;
            for(; t.nodeType !== 3;)if ((t.nodeType !== 1 || t.nodeName !== "INPUT" || t.type !== "hidden") && !i || (t = ke(t.nextSibling), t === null)) return null;
            return t;
        }
        function Ip(t, e) {
            for(; t.nodeType !== 8;)if ((t.nodeType !== 1 || t.nodeName !== "INPUT" || t.type !== "hidden") && !e || (t = ke(t.nextSibling), t === null)) return null;
            return t;
        }
        function fc(t) {
            return t.data === "$?" || t.data === "$~";
        }
        function dc(t) {
            return t.data === "$!" || t.data === "$?" && t.ownerDocument.readyState !== "loading";
        }
        function kS(t, e) {
            var i = t.ownerDocument;
            if (t.data === "$~") t._reactRetry = e;
            else if (t.data !== "$?" || i.readyState !== "loading") e();
            else {
                var s = function() {
                    e(), i.removeEventListener("DOMContentLoaded", s);
                };
                i.addEventListener("DOMContentLoaded", s), t._reactRetry = s;
            }
        }
        function ke(t) {
            for(; t != null; t = t.nextSibling){
                var e = t.nodeType;
                if (e === 1 || e === 3) break;
                if (e === 8) {
                    if (e = t.data, e === "$" || e === "$!" || e === "$?" || e === "$~" || e === "&" || e === "F!" || e === "F") break;
                    if (e === "/$" || e === "/&") return null;
                }
            }
            return t;
        }
        var hc = null;
        function tg(t) {
            t = t.nextSibling;
            for(var e = 0; t;){
                if (t.nodeType === 8) {
                    var i = t.data;
                    if (i === "/$" || i === "/&") {
                        if (e === 0) return ke(t.nextSibling);
                        e--;
                    } else i !== "$" && i !== "$!" && i !== "$?" && i !== "$~" && i !== "&" || e++;
                }
                t = t.nextSibling;
            }
            return null;
        }
        function eg(t) {
            t = t.previousSibling;
            for(var e = 0; t;){
                if (t.nodeType === 8) {
                    var i = t.data;
                    if (i === "$" || i === "$!" || i === "$?" || i === "$~" || i === "&") {
                        if (e === 0) return t;
                        e--;
                    } else i !== "/$" && i !== "/&" || e++;
                }
                t = t.previousSibling;
            }
            return null;
        }
        function ng(t, e, i) {
            switch(e = rr(i), t){
                case "html":
                    if (t = e.documentElement, !t) throw Error(r(452));
                    return t;
                case "head":
                    if (t = e.head, !t) throw Error(r(453));
                    return t;
                case "body":
                    if (t = e.body, !t) throw Error(r(454));
                    return t;
                default:
                    throw Error(r(451));
            }
        }
        function Sl(t) {
            for(var e = t.attributes; e.length;)t.removeAttributeNode(e[0]);
            po(t);
        }
        var Ye = new Map, ag = new Set;
        function or(t) {
            return typeof t.getRootNode == "function" ? t.getRootNode() : t.nodeType === 9 ? t : t.ownerDocument;
        }
        var _n = F.d;
        F.d = {
            f: YS,
            r: XS,
            D: KS,
            C: PS,
            L: ZS,
            m: QS,
            X: $S,
            S: FS,
            M: JS
        };
        function YS() {
            var t = _n.f(), e = Is();
            return t || e;
        }
        function XS(t) {
            var e = Ua(t);
            e !== null && e.tag === 5 && e.type === "form" ? xm(e) : _n.r(t);
        }
        var gi = typeof document > "u" ? null : document;
        function ig(t, e, i) {
            var s = gi;
            if (s && typeof e == "string" && e) {
                var o = Le(e);
                o = 'link[rel="' + t + '"][href="' + o + '"]', typeof i == "string" && (o += '[crossorigin="' + i + '"]'), ag.has(o) || (ag.add(o), t = {
                    rel: t,
                    crossOrigin: i,
                    href: e
                }, s.querySelector(o) === null && (e = s.createElement("link"), ue(e, "link", t), ne(e), s.head.appendChild(e)));
            }
        }
        function KS(t) {
            _n.D(t), ig("dns-prefetch", t, null);
        }
        function PS(t, e) {
            _n.C(t, e), ig("preconnect", t, e);
        }
        function ZS(t, e, i) {
            _n.L(t, e, i);
            var s = gi;
            if (s && t && e) {
                var o = 'link[rel="preload"][as="' + Le(e) + '"]';
                e === "image" && i && i.imageSrcSet ? (o += '[imagesrcset="' + Le(i.imageSrcSet) + '"]', typeof i.imageSizes == "string" && (o += '[imagesizes="' + Le(i.imageSizes) + '"]')) : o += '[href="' + Le(t) + '"]';
                var c = o;
                switch(e){
                    case "style":
                        c = yi(t);
                        break;
                    case "script":
                        c = vi(t);
                }
                Ye.has(c) || (t = v({
                    rel: "preload",
                    href: e === "image" && i && i.imageSrcSet ? void 0 : t,
                    as: e
                }, i), Ye.set(c, t), s.querySelector(o) !== null || e === "style" && s.querySelector(Tl(c)) || e === "script" && s.querySelector(El(c)) || (e = s.createElement("link"), ue(e, "link", t), ne(e), s.head.appendChild(e)));
            }
        }
        function QS(t, e) {
            _n.m(t, e);
            var i = gi;
            if (i && t) {
                var s = e && typeof e.as == "string" ? e.as : "script", o = 'link[rel="modulepreload"][as="' + Le(s) + '"][href="' + Le(t) + '"]', c = o;
                switch(s){
                    case "audioworklet":
                    case "paintworklet":
                    case "serviceworker":
                    case "sharedworker":
                    case "worker":
                    case "script":
                        c = vi(t);
                }
                if (!Ye.has(c) && (t = v({
                    rel: "modulepreload",
                    href: t
                }, e), Ye.set(c, t), i.querySelector(o) === null)) {
                    switch(s){
                        case "audioworklet":
                        case "paintworklet":
                        case "serviceworker":
                        case "sharedworker":
                        case "worker":
                        case "script":
                            if (i.querySelector(El(c))) return;
                    }
                    s = i.createElement("link"), ue(s, "link", t), ne(s), i.head.appendChild(s);
                }
            }
        }
        function FS(t, e, i) {
            _n.S(t, e, i);
            var s = gi;
            if (s && t) {
                var o = Ha(s).hoistableStyles, c = yi(t);
                e = e || "default";
                var g = o.get(c);
                if (!g) {
                    var b = {
                        loading: 0,
                        preload: null
                    };
                    if (g = s.querySelector(Tl(c))) b.loading = 5;
                    else {
                        t = v({
                            rel: "stylesheet",
                            href: t,
                            "data-precedence": e
                        }, i), (i = Ye.get(c)) && mc(t, i);
                        var C = g = s.createElement("link");
                        ne(C), ue(C, "link", t), C._p = new Promise(function(O, q) {
                            C.onload = O, C.onerror = q;
                        }), C.addEventListener("load", function() {
                            b.loading |= 1;
                        }), C.addEventListener("error", function() {
                            b.loading |= 2;
                        }), b.loading |= 4, ur(g, e, s);
                    }
                    g = {
                        type: "stylesheet",
                        instance: g,
                        count: 1,
                        state: b
                    }, o.set(c, g);
                }
            }
        }
        function $S(t, e) {
            _n.X(t, e);
            var i = gi;
            if (i && t) {
                var s = Ha(i).hoistableScripts, o = vi(t), c = s.get(o);
                c || (c = i.querySelector(El(o)), c || (t = v({
                    src: t,
                    async: !0
                }, e), (e = Ye.get(o)) && pc(t, e), c = i.createElement("script"), ne(c), ue(c, "link", t), i.head.appendChild(c)), c = {
                    type: "script",
                    instance: c,
                    count: 1,
                    state: null
                }, s.set(o, c));
            }
        }
        function JS(t, e) {
            _n.M(t, e);
            var i = gi;
            if (i && t) {
                var s = Ha(i).hoistableScripts, o = vi(t), c = s.get(o);
                c || (c = i.querySelector(El(o)), c || (t = v({
                    src: t,
                    async: !0,
                    type: "module"
                }, e), (e = Ye.get(o)) && pc(t, e), c = i.createElement("script"), ne(c), ue(c, "link", t), i.head.appendChild(c)), c = {
                    type: "script",
                    instance: c,
                    count: 1,
                    state: null
                }, s.set(o, c));
            }
        }
        function lg(t, e, i, s) {
            var o = (o = ot.current) ? or(o) : null;
            if (!o) throw Error(r(446));
            switch(t){
                case "meta":
                case "title":
                    return null;
                case "style":
                    return typeof i.precedence == "string" && typeof i.href == "string" ? (e = yi(i.href), i = Ha(o).hoistableStyles, s = i.get(e), s || (s = {
                        type: "style",
                        instance: null,
                        count: 0,
                        state: null
                    }, i.set(e, s)), s) : {
                        type: "void",
                        instance: null,
                        count: 0,
                        state: null
                    };
                case "link":
                    if (i.rel === "stylesheet" && typeof i.href == "string" && typeof i.precedence == "string") {
                        t = yi(i.href);
                        var c = Ha(o).hoistableStyles, g = c.get(t);
                        if (g || (o = o.ownerDocument || o, g = {
                            type: "stylesheet",
                            instance: null,
                            count: 0,
                            state: {
                                loading: 0,
                                preload: null
                            }
                        }, c.set(t, g), (c = o.querySelector(Tl(t))) && !c._p && (g.instance = c, g.state.loading = 5), Ye.has(t) || (i = {
                            rel: "preload",
                            as: "style",
                            href: i.href,
                            crossOrigin: i.crossOrigin,
                            integrity: i.integrity,
                            media: i.media,
                            hrefLang: i.hrefLang,
                            referrerPolicy: i.referrerPolicy
                        }, Ye.set(t, i), c || WS(o, t, i, g.state))), e && s === null) throw Error(r(528, ""));
                        return g;
                    }
                    if (e && s !== null) throw Error(r(529, ""));
                    return null;
                case "script":
                    return e = i.async, i = i.src, typeof i == "string" && e && typeof e != "function" && typeof e != "symbol" ? (e = vi(i), i = Ha(o).hoistableScripts, s = i.get(e), s || (s = {
                        type: "script",
                        instance: null,
                        count: 0,
                        state: null
                    }, i.set(e, s)), s) : {
                        type: "void",
                        instance: null,
                        count: 0,
                        state: null
                    };
                default:
                    throw Error(r(444, t));
            }
        }
        function yi(t) {
            return 'href="' + Le(t) + '"';
        }
        function Tl(t) {
            return 'link[rel="stylesheet"][' + t + "]";
        }
        function sg(t) {
            return v({}, t, {
                "data-precedence": t.precedence,
                precedence: null
            });
        }
        function WS(t, e, i, s) {
            t.querySelector('link[rel="preload"][as="style"][' + e + "]") ? s.loading = 1 : (e = t.createElement("link"), s.preload = e, e.addEventListener("load", function() {
                return s.loading |= 1;
            }), e.addEventListener("error", function() {
                return s.loading |= 2;
            }), ue(e, "link", i), ne(e), t.head.appendChild(e));
        }
        function vi(t) {
            return '[src="' + Le(t) + '"]';
        }
        function El(t) {
            return "script[async]" + t;
        }
        function rg(t, e, i) {
            if (e.count++, e.instance === null) switch(e.type){
                case "style":
                    var s = t.querySelector('style[data-href~="' + Le(i.href) + '"]');
                    if (s) return e.instance = s, ne(s), s;
                    var o = v({}, i, {
                        "data-href": i.href,
                        "data-precedence": i.precedence,
                        href: null,
                        precedence: null
                    });
                    return s = (t.ownerDocument || t).createElement("style"), ne(s), ue(s, "style", o), ur(s, i.precedence, t), e.instance = s;
                case "stylesheet":
                    o = yi(i.href);
                    var c = t.querySelector(Tl(o));
                    if (c) return e.state.loading |= 4, e.instance = c, ne(c), c;
                    s = sg(i), (o = Ye.get(o)) && mc(s, o), c = (t.ownerDocument || t).createElement("link"), ne(c);
                    var g = c;
                    return g._p = new Promise(function(b, C) {
                        g.onload = b, g.onerror = C;
                    }), ue(c, "link", s), e.state.loading |= 4, ur(c, i.precedence, t), e.instance = c;
                case "script":
                    return c = vi(i.src), (o = t.querySelector(El(c))) ? (e.instance = o, ne(o), o) : (s = i, (o = Ye.get(c)) && (s = v({}, i), pc(s, o)), t = t.ownerDocument || t, o = t.createElement("script"), ne(o), ue(o, "link", s), t.head.appendChild(o), e.instance = o);
                case "void":
                    return null;
                default:
                    throw Error(r(443, e.type));
            }
            else e.type === "stylesheet" && (e.state.loading & 4) === 0 && (s = e.instance, e.state.loading |= 4, ur(s, i.precedence, t));
            return e.instance;
        }
        function ur(t, e, i) {
            for(var s = i.querySelectorAll('link[rel="stylesheet"][data-precedence],style[data-precedence]'), o = s.length ? s[s.length - 1] : null, c = o, g = 0; g < s.length; g++){
                var b = s[g];
                if (b.dataset.precedence === e) c = b;
                else if (c !== o) break;
            }
            c ? c.parentNode.insertBefore(t, c.nextSibling) : (e = i.nodeType === 9 ? i.head : i, e.insertBefore(t, e.firstChild));
        }
        function mc(t, e) {
            t.crossOrigin == null && (t.crossOrigin = e.crossOrigin), t.referrerPolicy == null && (t.referrerPolicy = e.referrerPolicy), t.title == null && (t.title = e.title);
        }
        function pc(t, e) {
            t.crossOrigin == null && (t.crossOrigin = e.crossOrigin), t.referrerPolicy == null && (t.referrerPolicy = e.referrerPolicy), t.integrity == null && (t.integrity = e.integrity);
        }
        var cr = null;
        function og(t, e, i) {
            if (cr === null) {
                var s = new Map, o = cr = new Map;
                o.set(i, s);
            } else o = cr, s = o.get(i), s || (s = new Map, o.set(i, s));
            if (s.has(t)) return s;
            for(s.set(t, null), i = i.getElementsByTagName(t), o = 0; o < i.length; o++){
                var c = i[o];
                if (!(c[Hi] || c[le] || t === "link" && c.getAttribute("rel") === "stylesheet") && c.namespaceURI !== "http://www.w3.org/2000/svg") {
                    var g = c.getAttribute(e) || "";
                    g = t + g;
                    var b = s.get(g);
                    b ? b.push(c) : s.set(g, [
                        c
                    ]);
                }
            }
            return s;
        }
        function ug(t, e, i) {
            t = t.ownerDocument || t, t.head.insertBefore(i, e === "title" ? t.querySelector("head > title") : null);
        }
        function IS(t, e, i) {
            if (i === 1 || e.itemProp != null) return !1;
            switch(t){
                case "meta":
                case "title":
                    return !0;
                case "style":
                    if (typeof e.precedence != "string" || typeof e.href != "string" || e.href === "") break;
                    return !0;
                case "link":
                    if (typeof e.rel != "string" || typeof e.href != "string" || e.href === "" || e.onLoad || e.onError) break;
                    switch(e.rel){
                        case "stylesheet":
                            return t = e.disabled, typeof e.precedence == "string" && t == null;
                        default:
                            return !0;
                    }
                case "script":
                    if (e.async && typeof e.async != "function" && typeof e.async != "symbol" && !e.onLoad && !e.onError && e.src && typeof e.src == "string") return !0;
            }
            return !1;
        }
        function cg(t) {
            return !(t.type === "stylesheet" && (t.state.loading & 3) === 0);
        }
        function t1(t, e, i, s) {
            if (i.type === "stylesheet" && (typeof s.media != "string" || matchMedia(s.media).matches !== !1) && (i.state.loading & 4) === 0) {
                if (i.instance === null) {
                    var o = yi(s.href), c = e.querySelector(Tl(o));
                    if (c) {
                        e = c._p, e !== null && typeof e == "object" && typeof e.then == "function" && (t.count++, t = fr.bind(t), e.then(t, t)), i.state.loading |= 4, i.instance = c, ne(c);
                        return;
                    }
                    c = e.ownerDocument || e, s = sg(s), (o = Ye.get(o)) && mc(s, o), c = c.createElement("link"), ne(c);
                    var g = c;
                    g._p = new Promise(function(b, C) {
                        g.onload = b, g.onerror = C;
                    }), ue(c, "link", s), i.instance = c;
                }
                t.stylesheets === null && (t.stylesheets = new Map), t.stylesheets.set(i, e), (e = i.state.preload) && (i.state.loading & 3) === 0 && (t.count++, i = fr.bind(t), e.addEventListener("load", i), e.addEventListener("error", i));
            }
        }
        var gc = 0;
        function e1(t, e) {
            return t.stylesheets && t.count === 0 && hr(t, t.stylesheets), 0 < t.count || 0 < t.imgCount ? function(i) {
                var s = setTimeout(function() {
                    if (t.stylesheets && hr(t, t.stylesheets), t.unsuspend) {
                        var c = t.unsuspend;
                        t.unsuspend = null, c();
                    }
                }, 6e4 + e);
                0 < t.imgBytes && gc === 0 && (gc = 62500 * LS());
                var o = setTimeout(function() {
                    if (t.waitingForImages = !1, t.count === 0 && (t.stylesheets && hr(t, t.stylesheets), t.unsuspend)) {
                        var c = t.unsuspend;
                        t.unsuspend = null, c();
                    }
                }, (t.imgBytes > gc ? 50 : 800) + e);
                return t.unsuspend = i, function() {
                    t.unsuspend = null, clearTimeout(s), clearTimeout(o);
                };
            } : null;
        }
        function fr() {
            if (this.count--, this.count === 0 && (this.imgCount === 0 || !this.waitingForImages)) {
                if (this.stylesheets) hr(this, this.stylesheets);
                else if (this.unsuspend) {
                    var t = this.unsuspend;
                    this.unsuspend = null, t();
                }
            }
        }
        var dr = null;
        function hr(t, e) {
            t.stylesheets = null, t.unsuspend !== null && (t.count++, dr = new Map, e.forEach(n1, t), dr = null, fr.call(t));
        }
        function n1(t, e) {
            if (!(e.state.loading & 4)) {
                var i = dr.get(t);
                if (i) var s = i.get(null);
                else {
                    i = new Map, dr.set(t, i);
                    for(var o = t.querySelectorAll("link[data-precedence],style[data-precedence]"), c = 0; c < o.length; c++){
                        var g = o[c];
                        (g.nodeName === "LINK" || g.getAttribute("media") !== "not all") && (i.set(g.dataset.precedence, g), s = g);
                    }
                    s && i.set(null, s);
                }
                o = e.instance, g = o.getAttribute("data-precedence"), c = i.get(g) || s, c === s && i.set(null, o), i.set(g, o), this.count++, s = fr.bind(this), o.addEventListener("load", s), o.addEventListener("error", s), c ? c.parentNode.insertBefore(o, c.nextSibling) : (t = t.nodeType === 9 ? t.head : t, t.insertBefore(o, t.firstChild)), e.state.loading |= 4;
            }
        }
        var Al = {
            $$typeof: V,
            Provider: null,
            Consumer: null,
            _currentValue: $,
            _currentValue2: $,
            _threadCount: 0
        };
        function a1(t, e, i, s, o, c, g, b, C) {
            this.tag = 1, this.containerInfo = t, this.pingCache = this.current = this.pendingChildren = null, this.timeoutHandle = -1, this.callbackNode = this.next = this.pendingContext = this.context = this.cancelPendingCommit = null, this.callbackPriority = 0, this.expirationTimes = co(-1), this.entangledLanes = this.shellSuspendCounter = this.errorRecoveryDisabledLanes = this.expiredLanes = this.warmLanes = this.pingedLanes = this.suspendedLanes = this.pendingLanes = 0, this.entanglements = co(0), this.hiddenUpdates = co(null), this.identifierPrefix = s, this.onUncaughtError = o, this.onCaughtError = c, this.onRecoverableError = g, this.pooledCache = null, this.pooledCacheLanes = 0, this.formState = C, this.incompleteTransitions = new Map;
        }
        function fg(t, e, i, s, o, c, g, b, C, O, q, K) {
            return t = new a1(t, e, i, g, C, O, q, K, b), e = 1, c === !0 && (e |= 24), c = Re(3, null, null, e), t.current = c, c.stateNode = t, e = Fo(), e.refCount++, t.pooledCache = e, e.refCount++, c.memoizedState = {
                element: s,
                isDehydrated: i,
                cache: e
            }, Io(c), t;
        }
        function dg(t) {
            return t ? (t = Fa, t) : Fa;
        }
        function hg(t, e, i, s, o, c) {
            o = dg(o), s.context === null ? s.context = o : s.pendingContext = o, s = kn(e), s.payload = {
                element: i
            }, c = c === void 0 ? null : c, c !== null && (s.callback = c), i = Yn(t, s, e), i !== null && (Ee(i, t, e), nl(i, t, e));
        }
        function mg(t, e) {
            if (t = t.memoizedState, t !== null && t.dehydrated !== null) {
                var i = t.retryLane;
                t.retryLane = i !== 0 && i < e ? i : e;
            }
        }
        function yc(t, e) {
            mg(t, e), (t = t.alternate) && mg(t, e);
        }
        function pg(t) {
            if (t.tag === 13 || t.tag === 31) {
                var e = ya(t, 67108864);
                e !== null && Ee(e, t, 67108864), yc(t, 67108864);
            }
        }
        function gg(t) {
            if (t.tag === 13 || t.tag === 31) {
                var e = Ne();
                e = fo(e);
                var i = ya(t, e);
                i !== null && Ee(i, t, e), yc(t, e);
            }
        }
        var mr = !0;
        function i1(t, e, i, s) {
            var o = G.T;
            G.T = null;
            var c = F.p;
            try {
                F.p = 2, vc(t, e, i, s);
            } finally{
                F.p = c, G.T = o;
            }
        }
        function l1(t, e, i, s) {
            var o = G.T;
            G.T = null;
            var c = F.p;
            try {
                F.p = 8, vc(t, e, i, s);
            } finally{
                F.p = c, G.T = o;
            }
        }
        function vc(t, e, i, s) {
            if (mr) {
                var o = bc(s);
                if (o === null) ic(t, e, s, pr, i), vg(t, s);
                else if (r1(o, t, e, i, s)) s.stopPropagation();
                else if (vg(t, s), e & 4 && -1 < s1.indexOf(t)) {
                    for(; o !== null;){
                        var c = Ua(o);
                        if (c !== null) switch(c.tag){
                            case 3:
                                if (c = c.stateNode, c.current.memoizedState.isDehydrated) {
                                    var g = da(c.pendingLanes);
                                    if (g !== 0) {
                                        var b = c;
                                        for(b.pendingLanes |= 2, b.entangledLanes |= 2; g;){
                                            var C = 1 << 31 - we(g);
                                            b.entanglements[1] |= C, g &= ~C;
                                        }
                                        an(c), (Et & 6) === 0 && (Js = Ae() + 500, vl(0));
                                    }
                                }
                                break;
                            case 31:
                            case 13:
                                b = ya(c, 2), b !== null && Ee(b, c, 2), Is(), yc(c, 2);
                        }
                        if (c = bc(s), c === null && ic(t, e, s, pr, i), c === o) break;
                        o = c;
                    }
                    o !== null && s.stopPropagation();
                } else ic(t, e, s, null, i);
            }
        }
        function bc(t) {
            return t = So(t), xc(t);
        }
        var pr = null;
        function xc(t) {
            if (pr = null, t = Ba(t), t !== null) {
                var e = d(t);
                if (e === null) t = null;
                else {
                    var i = e.tag;
                    if (i === 13) {
                        if (t = f(e), t !== null) return t;
                        t = null;
                    } else if (i === 31) {
                        if (t = h(e), t !== null) return t;
                        t = null;
                    } else if (i === 3) {
                        if (e.stateNode.current.memoizedState.isDehydrated) return e.tag === 3 ? e.stateNode.containerInfo : null;
                        t = null;
                    } else e !== t && (t = null);
                }
            }
            return pr = t, null;
        }
        function yg(t) {
            switch(t){
                case "beforetoggle":
                case "cancel":
                case "click":
                case "close":
                case "contextmenu":
                case "copy":
                case "cut":
                case "auxclick":
                case "dblclick":
                case "dragend":
                case "dragstart":
                case "drop":
                case "focusin":
                case "focusout":
                case "input":
                case "invalid":
                case "keydown":
                case "keypress":
                case "keyup":
                case "mousedown":
                case "mouseup":
                case "paste":
                case "pause":
                case "play":
                case "pointercancel":
                case "pointerdown":
                case "pointerup":
                case "ratechange":
                case "reset":
                case "resize":
                case "seeked":
                case "submit":
                case "toggle":
                case "touchcancel":
                case "touchend":
                case "touchstart":
                case "volumechange":
                case "change":
                case "selectionchange":
                case "textInput":
                case "compositionstart":
                case "compositionend":
                case "compositionupdate":
                case "beforeblur":
                case "afterblur":
                case "beforeinput":
                case "blur":
                case "fullscreenchange":
                case "focus":
                case "hashchange":
                case "popstate":
                case "select":
                case "selectstart":
                    return 2;
                case "drag":
                case "dragenter":
                case "dragexit":
                case "dragleave":
                case "dragover":
                case "mousemove":
                case "mouseout":
                case "mouseover":
                case "pointermove":
                case "pointerout":
                case "pointerover":
                case "scroll":
                case "touchmove":
                case "wheel":
                case "mouseenter":
                case "mouseleave":
                case "pointerenter":
                case "pointerleave":
                    return 8;
                case "message":
                    switch(Kb()){
                        case Cd:
                            return 2;
                        case wd:
                            return 8;
                        case is:
                        case Pb:
                            return 32;
                        case _d:
                            return 268435456;
                        default:
                            return 32;
                    }
                default:
                    return 32;
            }
        }
        var Sc = !1, ta = null, ea = null, na = null, Cl = new Map, wl = new Map, aa = [], s1 = "mousedown mouseup touchcancel touchend touchstart auxclick dblclick pointercancel pointerdown pointerup dragend dragstart drop compositionend compositionstart keydown keypress keyup input textInput copy cut paste click change contextmenu reset".split(" ");
        function vg(t, e) {
            switch(t){
                case "focusin":
                case "focusout":
                    ta = null;
                    break;
                case "dragenter":
                case "dragleave":
                    ea = null;
                    break;
                case "mouseover":
                case "mouseout":
                    na = null;
                    break;
                case "pointerover":
                case "pointerout":
                    Cl.delete(e.pointerId);
                    break;
                case "gotpointercapture":
                case "lostpointercapture":
                    wl.delete(e.pointerId);
            }
        }
        function _l(t, e, i, s, o, c) {
            return t === null || t.nativeEvent !== c ? (t = {
                blockedOn: e,
                domEventName: i,
                eventSystemFlags: s,
                nativeEvent: c,
                targetContainers: [
                    o
                ]
            }, e !== null && (e = Ua(e), e !== null && pg(e)), t) : (t.eventSystemFlags |= s, e = t.targetContainers, o !== null && e.indexOf(o) === -1 && e.push(o), t);
        }
        function r1(t, e, i, s, o) {
            switch(e){
                case "focusin":
                    return ta = _l(ta, t, e, i, s, o), !0;
                case "dragenter":
                    return ea = _l(ea, t, e, i, s, o), !0;
                case "mouseover":
                    return na = _l(na, t, e, i, s, o), !0;
                case "pointerover":
                    var c = o.pointerId;
                    return Cl.set(c, _l(Cl.get(c) || null, t, e, i, s, o)), !0;
                case "gotpointercapture":
                    return c = o.pointerId, wl.set(c, _l(wl.get(c) || null, t, e, i, s, o)), !0;
            }
            return !1;
        }
        function bg(t) {
            var e = Ba(t.target);
            if (e !== null) {
                var i = d(e);
                if (i !== null) {
                    if (e = i.tag, e === 13) {
                        if (e = f(i), e !== null) {
                            t.blockedOn = e, Nd(t.priority, function() {
                                gg(i);
                            });
                            return;
                        }
                    } else if (e === 31) {
                        if (e = h(i), e !== null) {
                            t.blockedOn = e, Nd(t.priority, function() {
                                gg(i);
                            });
                            return;
                        }
                    } else if (e === 3 && i.stateNode.current.memoizedState.isDehydrated) {
                        t.blockedOn = i.tag === 3 ? i.stateNode.containerInfo : null;
                        return;
                    }
                }
            }
            t.blockedOn = null;
        }
        function gr(t) {
            if (t.blockedOn !== null) return !1;
            for(var e = t.targetContainers; 0 < e.length;){
                var i = bc(t.nativeEvent);
                if (i === null) {
                    i = t.nativeEvent;
                    var s = new i.constructor(i.type, i);
                    xo = s, i.target.dispatchEvent(s), xo = null;
                } else return e = Ua(i), e !== null && pg(e), t.blockedOn = i, !1;
                e.shift();
            }
            return !0;
        }
        function xg(t, e, i) {
            gr(t) && i.delete(e);
        }
        function o1() {
            Sc = !1, ta !== null && gr(ta) && (ta = null), ea !== null && gr(ea) && (ea = null), na !== null && gr(na) && (na = null), Cl.forEach(xg), wl.forEach(xg);
        }
        function yr(t, e) {
            t.blockedOn === e && (t.blockedOn = null, Sc || (Sc = !0, n.unstable_scheduleCallback(n.unstable_NormalPriority, o1)));
        }
        var vr = null;
        function Sg(t) {
            vr !== t && (vr = t, n.unstable_scheduleCallback(n.unstable_NormalPriority, function() {
                vr === t && (vr = null);
                for(var e = 0; e < t.length; e += 3){
                    var i = t[e], s = t[e + 1], o = t[e + 2];
                    if (typeof s != "function") {
                        if (xc(s || i) === null) continue;
                        break;
                    }
                    var c = Ua(i);
                    c !== null && (t.splice(e, 3), e -= 3, bu(c, {
                        pending: !0,
                        data: o,
                        method: i.method,
                        action: s
                    }, s, o));
                }
            }));
        }
        function bi(t) {
            function e(C) {
                return yr(C, t);
            }
            ta !== null && yr(ta, t), ea !== null && yr(ea, t), na !== null && yr(na, t), Cl.forEach(e), wl.forEach(e);
            for(var i = 0; i < aa.length; i++){
                var s = aa[i];
                s.blockedOn === t && (s.blockedOn = null);
            }
            for(; 0 < aa.length && (i = aa[0], i.blockedOn === null);)bg(i), i.blockedOn === null && aa.shift();
            if (i = (t.ownerDocument || t).$$reactFormReplay, i != null) for(s = 0; s < i.length; s += 3){
                var o = i[s], c = i[s + 1], g = o[ye] || null;
                if (typeof c == "function") g || Sg(i);
                else if (g) {
                    var b = null;
                    if (c && c.hasAttribute("formAction")) {
                        if (o = c, g = c[ye] || null) b = g.formAction;
                        else if (xc(o) !== null) continue;
                    } else b = g.action;
                    typeof b == "function" ? i[s + 1] = b : (i.splice(s, 3), s -= 3), Sg(i);
                }
            }
        }
        function Tg() {
            function t(c) {
                c.canIntercept && c.info === "react-transition" && c.intercept({
                    handler: function() {
                        return new Promise(function(g) {
                            return o = g;
                        });
                    },
                    focusReset: "manual",
                    scroll: "manual"
                });
            }
            function e() {
                o !== null && (o(), o = null), s || setTimeout(i, 20);
            }
            function i() {
                if (!s && !navigation.transition) {
                    var c = navigation.currentEntry;
                    c && c.url != null && navigation.navigate(c.url, {
                        state: c.getState(),
                        info: "react-transition",
                        history: "replace"
                    });
                }
            }
            if (typeof navigation == "object") {
                var s = !1, o = null;
                return navigation.addEventListener("navigate", t), navigation.addEventListener("navigatesuccess", e), navigation.addEventListener("navigateerror", e), setTimeout(i, 100), function() {
                    s = !0, navigation.removeEventListener("navigate", t), navigation.removeEventListener("navigatesuccess", e), navigation.removeEventListener("navigateerror", e), o !== null && (o(), o = null);
                };
            }
        }
        function Tc(t) {
            this._internalRoot = t;
        }
        br.prototype.render = Tc.prototype.render = function(t) {
            var e = this._internalRoot;
            if (e === null) throw Error(r(409));
            var i = e.current, s = Ne();
            hg(i, s, t, e, null, null);
        }, br.prototype.unmount = Tc.prototype.unmount = function() {
            var t = this._internalRoot;
            if (t !== null) {
                this._internalRoot = null;
                var e = t.containerInfo;
                hg(t.current, 2, null, t, null, null), Is(), e[Va] = null;
            }
        };
        function br(t) {
            this._internalRoot = t;
        }
        br.prototype.unstable_scheduleHydration = function(t) {
            if (t) {
                var e = Od();
                t = {
                    blockedOn: null,
                    target: t,
                    priority: e
                };
                for(var i = 0; i < aa.length && e !== 0 && e < aa[i].priority; i++);
                aa.splice(i, 0, t), i === 0 && bg(t);
            }
        };
        var Eg = a.version;
        if (Eg !== "19.2.4") throw Error(r(527, Eg, "19.2.4"));
        F.findDOMNode = function(t) {
            var e = t._reactInternals;
            if (e === void 0) throw typeof t.render == "function" ? Error(r(188)) : (t = Object.keys(t).join(","), Error(r(268, t)));
            return t = p(e), t = t !== null ? y(t) : null, t = t === null ? null : t.stateNode, t;
        };
        var u1 = {
            bundleType: 0,
            version: "19.2.4",
            rendererPackageName: "react-dom",
            currentDispatcherRef: G,
            reconcilerVersion: "19.2.4"
        };
        if (typeof __REACT_DEVTOOLS_GLOBAL_HOOK__ < "u") {
            var xr = __REACT_DEVTOOLS_GLOBAL_HOOK__;
            if (!xr.isDisabled && xr.supportsFiber) try {
                Vi = xr.inject(u1), Ce = xr;
            } catch  {}
        }
        return Ml.createRoot = function(t, e) {
            if (!u(t)) throw Error(r(299));
            var i = !1, s = "", o = Dm, c = jm, g = Om;
            return e != null && (e.unstable_strictMode === !0 && (i = !0), e.identifierPrefix !== void 0 && (s = e.identifierPrefix), e.onUncaughtError !== void 0 && (o = e.onUncaughtError), e.onCaughtError !== void 0 && (c = e.onCaughtError), e.onRecoverableError !== void 0 && (g = e.onRecoverableError)), e = fg(t, 1, !1, null, null, i, s, null, o, c, g, Tg), t[Va] = e.current, ac(t), new Tc(e);
        }, Ml.hydrateRoot = function(t, e, i) {
            if (!u(t)) throw Error(r(299));
            var s = !1, o = "", c = Dm, g = jm, b = Om, C = null;
            return i != null && (i.unstable_strictMode === !0 && (s = !0), i.identifierPrefix !== void 0 && (o = i.identifierPrefix), i.onUncaughtError !== void 0 && (c = i.onUncaughtError), i.onCaughtError !== void 0 && (g = i.onCaughtError), i.onRecoverableError !== void 0 && (b = i.onRecoverableError), i.formState !== void 0 && (C = i.formState)), e = fg(t, 1, !0, e, i ?? null, s, o, C, c, g, b, Tg), e.context = dg(null), i = e.current, s = Ne(), s = fo(s), o = kn(s), o.callback = null, Yn(i, o, s), i = s, e.current.lanes = i, Ui(e, i), an(e), t[Va] = e.current, ac(t), new br(e);
        }, Ml.version = "19.2.4", Ml;
    }
    var Ng;
    function x1() {
        if (Ng) return Cc.exports;
        Ng = 1;
        function n() {
            if (!(typeof __REACT_DEVTOOLS_GLOBAL_HOOK__ > "u" || typeof __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE != "function")) try {
                __REACT_DEVTOOLS_GLOBAL_HOOK__.checkDCE(n);
            } catch (a) {
                console.error(a);
            }
        }
        return n(), Cc.exports = b1(), Cc.exports;
    }
    var S1 = x1();
    var zg = "popstate";
    function Lg(n) {
        return typeof n == "object" && n != null && "pathname" in n && "search" in n && "hash" in n && "state" in n && "key" in n;
    }
    function T1(n = {}) {
        function a(r, u) {
            let d = u.state?.masked, { pathname: f, search: h, hash: m } = d || r.location;
            return af("", {
                pathname: f,
                search: h,
                hash: m
            }, u.state && u.state.usr || null, u.state && u.state.key || "default", d ? {
                pathname: r.location.pathname,
                search: r.location.search,
                hash: r.location.hash
            } : void 0);
        }
        function l(r, u) {
            return typeof u == "string" ? u : kl(u);
        }
        return A1(a, l, null, n);
    }
    function Ht(n, a) {
        if (n === !1 || n === null || typeof n > "u") throw new Error(a);
    }
    function on(n, a) {
        if (!n) {
            typeof console < "u" && console.warn(a);
            try {
                throw new Error(a);
            } catch  {}
        }
    }
    function E1() {
        return Math.random().toString(36).substring(2, 10);
    }
    function Vg(n, a) {
        return {
            usr: n.state,
            key: n.key,
            idx: a,
            masked: n.unstable_mask ? {
                pathname: n.pathname,
                search: n.search,
                hash: n.hash
            } : void 0
        };
    }
    function af(n, a, l = null, r, u) {
        return {
            pathname: typeof n == "string" ? n : n.pathname,
            search: "",
            hash: "",
            ...typeof a == "string" ? Di(a) : a,
            state: l,
            key: a && a.key || r || E1(),
            unstable_mask: u
        };
    }
    function kl({ pathname: n = "/", search: a = "", hash: l = "" }) {
        return a && a !== "?" && (n += a.charAt(0) === "?" ? a : "?" + a), l && l !== "#" && (n += l.charAt(0) === "#" ? l : "#" + l), n;
    }
    function Di(n) {
        let a = {};
        if (n) {
            let l = n.indexOf("#");
            l >= 0 && (a.hash = n.substring(l), n = n.substring(0, l));
            let r = n.indexOf("?");
            r >= 0 && (a.search = n.substring(r), n = n.substring(0, r)), n && (a.pathname = n);
        }
        return a;
    }
    function A1(n, a, l, r = {}) {
        let { window: u = document.defaultView, v5Compat: d = !1 } = r, f = u.history, h = "POP", m = null, p = y();
        p == null && (p = 0, f.replaceState({
            ...f.state,
            idx: p
        }, ""));
        function y() {
            return (f.state || {
                idx: null
            }).idx;
        }
        function v() {
            h = "POP";
            let R = y(), z = R == null ? null : R - p;
            p = R, m && m({
                action: h,
                location: M.location,
                delta: z
            });
        }
        function x(R, z) {
            h = "PUSH";
            let B = Lg(R) ? R : af(M.location, R, z);
            p = y() + 1;
            let V = Vg(B, p), P = M.createHref(B.unstable_mask || B);
            try {
                f.pushState(V, "", P);
            } catch (U) {
                if (U instanceof DOMException && U.name === "DataCloneError") throw U;
                u.location.assign(P);
            }
            d && m && m({
                action: h,
                location: M.location,
                delta: 1
            });
        }
        function A(R, z) {
            h = "REPLACE";
            let B = Lg(R) ? R : af(M.location, R, z);
            p = y();
            let V = Vg(B, p), P = M.createHref(B.unstable_mask || B);
            f.replaceState(V, "", P), d && m && m({
                action: h,
                location: M.location,
                delta: 0
            });
        }
        function E(R) {
            return C1(R);
        }
        let M = {
            get action () {
                return h;
            },
            get location () {
                return n(u, f);
            },
            listen (R) {
                if (m) throw new Error("A history only accepts one active listener");
                return u.addEventListener(zg, v), m = R, ()=>{
                    u.removeEventListener(zg, v), m = null;
                };
            },
            createHref (R) {
                return a(u, R);
            },
            createURL: E,
            encodeLocation (R) {
                let z = E(R);
                return {
                    pathname: z.pathname,
                    search: z.search,
                    hash: z.hash
                };
            },
            push: x,
            replace: A,
            go (R) {
                return f.go(R);
            }
        };
        return M;
    }
    function C1(n, a = !1) {
        let l = "http://localhost";
        typeof window < "u" && (l = window.location.origin !== "null" ? window.location.origin : window.location.href), Ht(l, "No window.location.(origin|href) available to create URL");
        let r = typeof n == "string" ? n : kl(n);
        return r = r.replace(/ $/, "%20"), !a && r.startsWith("//") && (r = l + r), new URL(r, l);
    }
    function x0(n, a, l = "/") {
        return w1(n, a, l, !1);
    }
    function w1(n, a, l, r) {
        let u = typeof a == "string" ? Di(a) : a, d = Mn(u.pathname || "/", l);
        if (d == null) return null;
        let f = S0(n);
        _1(f);
        let h = null;
        for(let m = 0; h == null && m < f.length; ++m){
            let p = U1(d);
            h = V1(f[m], p, r);
        }
        return h;
    }
    function S0(n, a = [], l = [], r = "", u = !1) {
        let d = (f, h, m = u, p)=>{
            let y = {
                relativePath: p === void 0 ? f.path || "" : p,
                caseSensitive: f.caseSensitive === !0,
                childrenIndex: h,
                route: f
            };
            if (y.relativePath.startsWith("/")) {
                if (!y.relativePath.startsWith(r) && m) return;
                Ht(y.relativePath.startsWith(r), `Absolute route path "${y.relativePath}" nested under path "${r}" is not valid. An absolute child route path must start with the combined path of all its parent routes.`), y.relativePath = y.relativePath.slice(r.length);
            }
            let v = sn([
                r,
                y.relativePath
            ]), x = l.concat(y);
            f.children && f.children.length > 0 && (Ht(f.index !== !0, `Index routes must not have child routes. Please remove all child routes from route path "${v}".`), S0(f.children, a, x, v, m)), !(f.path == null && !f.index) && a.push({
                path: v,
                score: z1(v, f.index),
                routesMeta: x
            });
        };
        return n.forEach((f, h)=>{
            if (f.path === "" || !f.path?.includes("?")) d(f, h);
            else for (let m of T0(f.path))d(f, h, !0, m);
        }), a;
    }
    function T0(n) {
        let a = n.split("/");
        if (a.length === 0) return [];
        let [l, ...r] = a, u = l.endsWith("?"), d = l.replace(/\?$/, "");
        if (r.length === 0) return u ? [
            d,
            ""
        ] : [
            d
        ];
        let f = T0(r.join("/")), h = [];
        return h.push(...f.map((m)=>m === "" ? d : [
                d,
                m
            ].join("/"))), u && h.push(...f), h.map((m)=>n.startsWith("/") && m === "" ? "/" : m);
    }
    function _1(n) {
        n.sort((a, l)=>a.score !== l.score ? l.score - a.score : L1(a.routesMeta.map((r)=>r.childrenIndex), l.routesMeta.map((r)=>r.childrenIndex)));
    }
    var R1 = /^:[\w-]+$/, M1 = 3, D1 = 2, j1 = 1, O1 = 10, N1 = -2, Bg = (n)=>n === "*";
    function z1(n, a) {
        let l = n.split("/"), r = l.length;
        return l.some(Bg) && (r += N1), a && (r += D1), l.filter((u)=>!Bg(u)).reduce((u, d)=>u + (R1.test(d) ? M1 : d === "" ? j1 : O1), r);
    }
    function L1(n, a) {
        return n.length === a.length && n.slice(0, -1).every((r, u)=>r === a[u]) ? n[n.length - 1] - a[a.length - 1] : 0;
    }
    function V1(n, a, l = !1) {
        let { routesMeta: r } = n, u = {}, d = "/", f = [];
        for(let h = 0; h < r.length; ++h){
            let m = r[h], p = h === r.length - 1, y = d === "/" ? a : a.slice(d.length) || "/", v = qr({
                path: m.relativePath,
                caseSensitive: m.caseSensitive,
                end: p
            }, y), x = m.route;
            if (!v && p && l && !r[r.length - 1].route.index && (v = qr({
                path: m.relativePath,
                caseSensitive: m.caseSensitive,
                end: !1
            }, y)), !v) return null;
            Object.assign(u, v.params), f.push({
                params: u,
                pathname: sn([
                    d,
                    v.pathname
                ]),
                pathnameBase: k1(sn([
                    d,
                    v.pathnameBase
                ])),
                route: x
            }), v.pathnameBase !== "/" && (d = sn([
                d,
                v.pathnameBase
            ]));
        }
        return f;
    }
    function qr(n, a) {
        typeof n == "string" && (n = {
            path: n,
            caseSensitive: !1,
            end: !0
        });
        let [l, r] = B1(n.path, n.caseSensitive, n.end), u = a.match(l);
        if (!u) return null;
        let d = u[0], f = d.replace(/(.)\/+$/, "$1"), h = u.slice(1);
        return {
            params: r.reduce((p, { paramName: y, isOptional: v }, x)=>{
                if (y === "*") {
                    let E = h[x] || "";
                    f = d.slice(0, d.length - E.length).replace(/(.)\/+$/, "$1");
                }
                const A = h[x];
                return v && !A ? p[y] = void 0 : p[y] = (A || "").replace(/%2F/g, "/"), p;
            }, {}),
            pathname: d,
            pathnameBase: f,
            pattern: n
        };
    }
    function B1(n, a = !1, l = !0) {
        on(n === "*" || !n.endsWith("*") || n.endsWith("/*"), `Route path "${n}" will be treated as if it were "${n.replace(/\*$/, "/*")}" because the \`*\` character must always follow a \`/\` in the pattern. To get rid of this warning, please change the route path to "${n.replace(/\*$/, "/*")}".`);
        let r = [], u = "^" + n.replace(/\/*\*?$/, "").replace(/^\/*/, "/").replace(/[\\.*+^${}|()[\]]/g, "\\$&").replace(/\/:([\w-]+)(\?)?/g, (f, h, m, p, y)=>{
            if (r.push({
                paramName: h,
                isOptional: m != null
            }), m) {
                let v = y.charAt(p + f.length);
                return v && v !== "/" ? "/([^\\/]*)" : "(?:/([^\\/]*))?";
            }
            return "/([^\\/]+)";
        }).replace(/\/([\w-]+)\?(\/|$)/g, "(/$1)?$2");
        return n.endsWith("*") ? (r.push({
            paramName: "*"
        }), u += n === "*" || n === "/*" ? "(.*)$" : "(?:\\/(.+)|\\/*)$") : l ? u += "\\/*$" : n !== "" && n !== "/" && (u += "(?:(?=\\/|$))"), [
            new RegExp(u, a ? void 0 : "i"),
            r
        ];
    }
    function U1(n) {
        try {
            return n.split("/").map((a)=>decodeURIComponent(a).replace(/\//g, "%2F")).join("/");
        } catch (a) {
            return on(!1, `The URL path "${n}" could not be decoded because it is a malformed URL segment. This is probably due to a bad percent encoding (${a}).`), n;
        }
    }
    function Mn(n, a) {
        if (a === "/") return n;
        if (!n.toLowerCase().startsWith(a.toLowerCase())) return null;
        let l = a.endsWith("/") ? a.length - 1 : a.length, r = n.charAt(l);
        return r && r !== "/" ? null : n.slice(l) || "/";
    }
    var H1 = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;
    function G1(n, a = "/") {
        let { pathname: l, search: r = "", hash: u = "" } = typeof n == "string" ? Di(n) : n, d;
        return l ? (l = l.replace(/\/\/+/g, "/"), l.startsWith("/") ? d = Ug(l.substring(1), "/") : d = Ug(l, a)) : d = a, {
            pathname: d,
            search: Y1(r),
            hash: X1(u)
        };
    }
    function Ug(n, a) {
        let l = a.replace(/\/+$/, "").split("/");
        return n.split("/").forEach((u)=>{
            u === ".." ? l.length > 1 && l.pop() : u !== "." && l.push(u);
        }), l.length > 1 ? l.join("/") : "/";
    }
    function Mc(n, a, l, r) {
        return `Cannot include a '${n}' character in a manually specified \`to.${a}\` field [${JSON.stringify(r)}].  Please separate it out to the \`to.${l}\` field. Alternatively you may provide the full path as a string in <Link to="..."> and the router will parse it for you.`;
    }
    function q1(n) {
        return n.filter((a, l)=>l === 0 || a.route.path && a.route.path.length > 0);
    }
    function E0(n) {
        let a = q1(n);
        return a.map((l, r)=>r === a.length - 1 ? l.pathname : l.pathnameBase);
    }
    function Nf(n, a, l, r = !1) {
        let u;
        typeof n == "string" ? u = Di(n) : (u = {
            ...n
        }, Ht(!u.pathname || !u.pathname.includes("?"), Mc("?", "pathname", "search", u)), Ht(!u.pathname || !u.pathname.includes("#"), Mc("#", "pathname", "hash", u)), Ht(!u.search || !u.search.includes("#"), Mc("#", "search", "hash", u)));
        let d = n === "" || u.pathname === "", f = d ? "/" : u.pathname, h;
        if (f == null) h = l;
        else {
            let v = a.length - 1;
            if (!r && f.startsWith("..")) {
                let x = f.split("/");
                for(; x[0] === "..";)x.shift(), v -= 1;
                u.pathname = x.join("/");
            }
            h = v >= 0 ? a[v] : "/";
        }
        let m = G1(u, h), p = f && f !== "/" && f.endsWith("/"), y = (d || f === ".") && l.endsWith("/");
        return !m.pathname.endsWith("/") && (p || y) && (m.pathname += "/"), m;
    }
    var sn = (n)=>n.join("/").replace(/\/\/+/g, "/"), k1 = (n)=>n.replace(/\/+$/, "").replace(/^\/*/, "/"), Y1 = (n)=>!n || n === "?" ? "" : n.startsWith("?") ? n : "?" + n, X1 = (n)=>!n || n === "#" ? "" : n.startsWith("#") ? n : "#" + n, K1 = class {
        constructor(n, a, l, r = !1){
            this.status = n, this.statusText = a || "", this.internal = r, l instanceof Error ? (this.data = l.toString(), this.error = l) : this.data = l;
        }
    };
    function P1(n) {
        return n != null && typeof n.status == "number" && typeof n.statusText == "string" && typeof n.internal == "boolean" && "data" in n;
    }
    function Z1(n) {
        return n.map((a)=>a.route.path).filter(Boolean).join("/").replace(/\/\/*/g, "/") || "/";
    }
    var A0 = typeof window < "u" && typeof window.document < "u" && typeof window.document.createElement < "u";
    function C0(n, a) {
        let l = n;
        if (typeof l != "string" || !H1.test(l)) return {
            absoluteURL: void 0,
            isExternal: !1,
            to: l
        };
        let r = l, u = !1;
        if (A0) try {
            let d = new URL(window.location.href), f = l.startsWith("//") ? new URL(d.protocol + l) : new URL(l), h = Mn(f.pathname, a);
            f.origin === d.origin && h != null ? l = h + f.search + f.hash : u = !0;
        } catch  {
            on(!1, `<Link to="${l}"> contains an invalid URL which will probably break when clicked - please update to a valid URL path.`);
        }
        return {
            absoluteURL: r,
            isExternal: u,
            to: l
        };
    }
    Object.getOwnPropertyNames(Object.prototype).sort().join("\0");
    var w0 = [
        "POST",
        "PUT",
        "PATCH",
        "DELETE"
    ];
    new Set(w0);
    var Q1 = [
        "GET",
        ...w0
    ];
    new Set(Q1);
    var ji = T.createContext(null);
    ji.displayName = "DataRouter";
    var $r = T.createContext(null);
    $r.displayName = "DataRouterState";
    var F1 = T.createContext(!1), _0 = T.createContext({
        isTransitioning: !1
    });
    _0.displayName = "ViewTransition";
    var $1 = T.createContext(new Map);
    $1.displayName = "Fetchers";
    var J1 = T.createContext(null);
    J1.displayName = "Await";
    var Ze = T.createContext(null);
    Ze.displayName = "Navigation";
    var Fl = T.createContext(null);
    Fl.displayName = "Location";
    var On = T.createContext({
        outlet: null,
        matches: [],
        isDataRoute: !1
    });
    On.displayName = "Route";
    var zf = T.createContext(null);
    zf.displayName = "RouteError";
    var R0 = "REACT_ROUTER_ERROR", W1 = "REDIRECT", I1 = "ROUTE_ERROR_RESPONSE";
    function tT(n) {
        if (n.startsWith(`${R0}:${W1}:{`)) try {
            let a = JSON.parse(n.slice(28));
            if (typeof a == "object" && a && typeof a.status == "number" && typeof a.statusText == "string" && typeof a.location == "string" && typeof a.reloadDocument == "boolean" && typeof a.replace == "boolean") return a;
        } catch  {}
    }
    function eT(n) {
        if (n.startsWith(`${R0}:${I1}:{`)) try {
            let a = JSON.parse(n.slice(40));
            if (typeof a == "object" && a && typeof a.status == "number" && typeof a.statusText == "string") return new K1(a.status, a.statusText, a.data);
        } catch  {}
    }
    function nT(n, { relative: a } = {}) {
        Ht($l(), "useHref() may be used only in the context of a <Router> component.");
        let { basename: l, navigator: r } = T.useContext(Ze), { hash: u, pathname: d, search: f } = Jl(n, {
            relative: a
        }), h = d;
        return l !== "/" && (h = d === "/" ? l : sn([
            l,
            d
        ])), r.createHref({
            pathname: h,
            search: f,
            hash: u
        });
    }
    function $l() {
        return T.useContext(Fl) != null;
    }
    function ua() {
        return Ht($l(), "useLocation() may be used only in the context of a <Router> component."), T.useContext(Fl).location;
    }
    var M0 = "You should call navigate() in a React.useEffect(), not when your component is first rendered.";
    function D0(n) {
        T.useContext(Ze).static || T.useLayoutEffect(n);
    }
    function Lf() {
        let { isDataRoute: n } = T.useContext(On);
        return n ? pT() : aT();
    }
    function aT() {
        Ht($l(), "useNavigate() may be used only in the context of a <Router> component.");
        let n = T.useContext(ji), { basename: a, navigator: l } = T.useContext(Ze), { matches: r } = T.useContext(On), { pathname: u } = ua(), d = JSON.stringify(E0(r)), f = T.useRef(!1);
        return D0(()=>{
            f.current = !0;
        }), T.useCallback((m, p = {})=>{
            if (on(f.current, M0), !f.current) return;
            if (typeof m == "number") {
                l.go(m);
                return;
            }
            let y = Nf(m, JSON.parse(d), u, p.relative === "path");
            n == null && a !== "/" && (y.pathname = y.pathname === "/" ? a : sn([
                a,
                y.pathname
            ])), (p.replace ? l.replace : l.push)(y, p.state, p);
        }, [
            a,
            l,
            d,
            u,
            n
        ]);
    }
    T.createContext(null);
    function Jl(n, { relative: a } = {}) {
        let { matches: l } = T.useContext(On), { pathname: r } = ua(), u = JSON.stringify(E0(l));
        return T.useMemo(()=>Nf(n, JSON.parse(u), r, a === "path"), [
            n,
            u,
            r,
            a
        ]);
    }
    function iT(n, a) {
        return j0(n, a);
    }
    function j0(n, a, l) {
        Ht($l(), "useRoutes() may be used only in the context of a <Router> component.");
        let { navigator: r } = T.useContext(Ze), { matches: u } = T.useContext(On), d = u[u.length - 1], f = d ? d.params : {}, h = d ? d.pathname : "/", m = d ? d.pathnameBase : "/", p = d && d.route;
        {
            let R = p && p.path || "";
            N0(h, !p || R.endsWith("*") || R.endsWith("*?"), `You rendered descendant <Routes> (or called \`useRoutes()\`) at "${h}" (under <Route path="${R}">) but the parent route path has no trailing "*". This means if you navigate deeper, the parent won't match anymore and therefore the child routes will never render.

Please change the parent <Route path="${R}"> to <Route path="${R === "/" ? "*" : `${R}/*`}">.`);
        }
        let y = ua(), v;
        if (a) {
            let R = typeof a == "string" ? Di(a) : a;
            Ht(m === "/" || R.pathname?.startsWith(m), `When overriding the location using \`<Routes location>\` or \`useRoutes(routes, location)\`, the location pathname must begin with the portion of the URL pathname that was matched by all parent routes. The current pathname base is "${m}" but pathname "${R.pathname}" was given in the \`location\` prop.`), v = R;
        } else v = y;
        let x = v.pathname || "/", A = x;
        if (m !== "/") {
            let R = m.replace(/^\//, "").split("/");
            A = "/" + x.replace(/^\//, "").split("/").slice(R.length).join("/");
        }
        let E = x0(n, {
            pathname: A
        });
        on(p || E != null, `No routes matched location "${v.pathname}${v.search}${v.hash}" `), on(E == null || E[E.length - 1].route.element !== void 0 || E[E.length - 1].route.Component !== void 0 || E[E.length - 1].route.lazy !== void 0, `Matched leaf route at location "${v.pathname}${v.search}${v.hash}" does not have an element or Component. This means it will render an <Outlet /> with a null value by default resulting in an "empty" page.`);
        let M = uT(E && E.map((R)=>Object.assign({}, R, {
                params: Object.assign({}, f, R.params),
                pathname: sn([
                    m,
                    r.encodeLocation ? r.encodeLocation(R.pathname.replace(/\?/g, "%3F").replace(/#/g, "%23")).pathname : R.pathname
                ]),
                pathnameBase: R.pathnameBase === "/" ? m : sn([
                    m,
                    r.encodeLocation ? r.encodeLocation(R.pathnameBase.replace(/\?/g, "%3F").replace(/#/g, "%23")).pathname : R.pathnameBase
                ])
            })), u, l);
        return a && M ? T.createElement(Fl.Provider, {
            value: {
                location: {
                    pathname: "/",
                    search: "",
                    hash: "",
                    state: null,
                    key: "default",
                    unstable_mask: void 0,
                    ...v
                },
                navigationType: "POP"
            }
        }, M) : M;
    }
    function lT() {
        let n = mT(), a = P1(n) ? `${n.status} ${n.statusText}` : n instanceof Error ? n.message : JSON.stringify(n), l = n instanceof Error ? n.stack : null, r = "rgba(200,200,200, 0.5)", u = {
            padding: "0.5rem",
            backgroundColor: r
        }, d = {
            padding: "2px 4px",
            backgroundColor: r
        }, f = null;
        return console.error("Error handled by React Router default ErrorBoundary:", n), f = T.createElement(T.Fragment, null, T.createElement("p", null, "💿 Hey developer 👋"), T.createElement("p", null, "You can provide a way better UX than this when your app throws errors by providing your own ", T.createElement("code", {
            style: d
        }, "ErrorBoundary"), " or", " ", T.createElement("code", {
            style: d
        }, "errorElement"), " prop on your route.")), T.createElement(T.Fragment, null, T.createElement("h2", null, "Unexpected Application Error!"), T.createElement("h3", {
            style: {
                fontStyle: "italic"
            }
        }, a), l ? T.createElement("pre", {
            style: u
        }, l) : null, f);
    }
    var sT = T.createElement(lT, null), O0 = class extends T.Component {
        constructor(n){
            super(n), this.state = {
                location: n.location,
                revalidation: n.revalidation,
                error: n.error
            };
        }
        static getDerivedStateFromError(n) {
            return {
                error: n
            };
        }
        static getDerivedStateFromProps(n, a) {
            return a.location !== n.location || a.revalidation !== "idle" && n.revalidation === "idle" ? {
                error: n.error,
                location: n.location,
                revalidation: n.revalidation
            } : {
                error: n.error !== void 0 ? n.error : a.error,
                location: a.location,
                revalidation: n.revalidation || a.revalidation
            };
        }
        componentDidCatch(n, a) {
            this.props.onError ? this.props.onError(n, a) : console.error("React Router caught the following error during render", n);
        }
        render() {
            let n = this.state.error;
            if (this.context && typeof n == "object" && n && "digest" in n && typeof n.digest == "string") {
                const l = eT(n.digest);
                l && (n = l);
            }
            let a = n !== void 0 ? T.createElement(On.Provider, {
                value: this.props.routeContext
            }, T.createElement(zf.Provider, {
                value: n,
                children: this.props.component
            })) : this.props.children;
            return this.context ? T.createElement(rT, {
                error: n
            }, a) : a;
        }
    };
    O0.contextType = F1;
    var Dc = new WeakMap;
    function rT({ children: n, error: a }) {
        let { basename: l } = T.useContext(Ze);
        if (typeof a == "object" && a && "digest" in a && typeof a.digest == "string") {
            let r = tT(a.digest);
            if (r) {
                let u = Dc.get(a);
                if (u) throw u;
                let d = C0(r.location, l);
                if (A0 && !Dc.get(a)) if (d.isExternal || r.reloadDocument) window.location.href = d.absoluteURL || d.to;
                else {
                    const f = Promise.resolve().then(()=>window.__reactRouterDataRouter.navigate(d.to, {
                            replace: r.replace
                        }));
                    throw Dc.set(a, f), f;
                }
                return T.createElement("meta", {
                    httpEquiv: "refresh",
                    content: `0;url=${d.absoluteURL || d.to}`
                });
            }
        }
        return n;
    }
    function oT({ routeContext: n, match: a, children: l }) {
        let r = T.useContext(ji);
        return r && r.static && r.staticContext && (a.route.errorElement || a.route.ErrorBoundary) && (r.staticContext._deepestRenderedBoundaryId = a.route.id), T.createElement(On.Provider, {
            value: n
        }, l);
    }
    function uT(n, a = [], l) {
        let r = l?.state;
        if (n == null) {
            if (!r) return null;
            if (r.errors) n = r.matches;
            else if (a.length === 0 && !r.initialized && r.matches.length > 0) n = r.matches;
            else return null;
        }
        let u = n, d = r?.errors;
        if (d != null) {
            let y = u.findIndex((v)=>v.route.id && d?.[v.route.id] !== void 0);
            Ht(y >= 0, `Could not find a matching route for errors on route IDs: ${Object.keys(d).join(",")}`), u = u.slice(0, Math.min(u.length, y + 1));
        }
        let f = !1, h = -1;
        if (l && r) {
            f = r.renderFallback;
            for(let y = 0; y < u.length; y++){
                let v = u[y];
                if ((v.route.HydrateFallback || v.route.hydrateFallbackElement) && (h = y), v.route.id) {
                    let { loaderData: x, errors: A } = r, E = v.route.loader && !x.hasOwnProperty(v.route.id) && (!A || A[v.route.id] === void 0);
                    if (v.route.lazy || E) {
                        l.isStatic && (f = !0), h >= 0 ? u = u.slice(0, h + 1) : u = [
                            u[0]
                        ];
                        break;
                    }
                }
            }
        }
        let m = l?.onError, p = r && m ? (y, v)=>{
            m(y, {
                location: r.location,
                params: r.matches?.[0]?.params ?? {},
                unstable_pattern: Z1(r.matches),
                errorInfo: v
            });
        } : void 0;
        return u.reduceRight((y, v, x)=>{
            let A, E = !1, M = null, R = null;
            r && (A = d && v.route.id ? d[v.route.id] : void 0, M = v.route.errorElement || sT, f && (h < 0 && x === 0 ? (N0("route-fallback", !1, "No `HydrateFallback` element provided to render during initial hydration"), E = !0, R = null) : h === x && (E = !0, R = v.route.hydrateFallbackElement || null)));
            let z = a.concat(u.slice(0, x + 1)), B = ()=>{
                let V;
                return A ? V = M : E ? V = R : v.route.Component ? V = T.createElement(v.route.Component, null) : v.route.element ? V = v.route.element : V = y, T.createElement(oT, {
                    match: v,
                    routeContext: {
                        outlet: y,
                        matches: z,
                        isDataRoute: r != null
                    },
                    children: V
                });
            };
            return r && (v.route.ErrorBoundary || v.route.errorElement || x === 0) ? T.createElement(O0, {
                location: r.location,
                revalidation: r.revalidation,
                component: M,
                error: A,
                children: B(),
                routeContext: {
                    outlet: null,
                    matches: z,
                    isDataRoute: !0
                },
                onError: p
            }) : B();
        }, null);
    }
    function Vf(n) {
        return `${n} must be used within a data router.  See https://reactrouter.com/en/main/routers/picking-a-router.`;
    }
    function cT(n) {
        let a = T.useContext(ji);
        return Ht(a, Vf(n)), a;
    }
    function fT(n) {
        let a = T.useContext($r);
        return Ht(a, Vf(n)), a;
    }
    function dT(n) {
        let a = T.useContext(On);
        return Ht(a, Vf(n)), a;
    }
    function Bf(n) {
        let a = dT(n), l = a.matches[a.matches.length - 1];
        return Ht(l.route.id, `${n} can only be used on routes that contain a unique "id"`), l.route.id;
    }
    function hT() {
        return Bf("useRouteId");
    }
    function mT() {
        let n = T.useContext(zf), a = fT("useRouteError"), l = Bf("useRouteError");
        return n !== void 0 ? n : a.errors?.[l];
    }
    function pT() {
        let { router: n } = cT("useNavigate"), a = Bf("useNavigate"), l = T.useRef(!1);
        return D0(()=>{
            l.current = !0;
        }), T.useCallback(async (u, d = {})=>{
            on(l.current, M0), l.current && (typeof u == "number" ? await n.navigate(u) : await n.navigate(u, {
                fromRouteId: a,
                ...d
            }));
        }, [
            n,
            a
        ]);
    }
    var Hg = {};
    function N0(n, a, l) {
        !a && !Hg[n] && (Hg[n] = !0, on(!1, l));
    }
    T.memo(gT);
    function gT({ routes: n, future: a, state: l, isStatic: r, onError: u }) {
        return j0(n, void 0, {
            state: l,
            isStatic: r,
            onError: u
        });
    }
    function Mr(n) {
        Ht(!1, "A <Route> is only ever to be used as the child of <Routes> element, never rendered directly. Please wrap your <Route> in a <Routes>.");
    }
    function yT({ basename: n = "/", children: a = null, location: l, navigationType: r = "POP", navigator: u, static: d = !1, unstable_useTransitions: f }) {
        Ht(!$l(), "You cannot render a <Router> inside another <Router>. You should never have more than one in your app.");
        let h = n.replace(/^\/*/, "/"), m = T.useMemo(()=>({
                basename: h,
                navigator: u,
                static: d,
                unstable_useTransitions: f,
                future: {}
            }), [
            h,
            u,
            d,
            f
        ]);
        typeof l == "string" && (l = Di(l));
        let { pathname: p = "/", search: y = "", hash: v = "", state: x = null, key: A = "default", unstable_mask: E } = l, M = T.useMemo(()=>{
            let R = Mn(p, h);
            return R == null ? null : {
                location: {
                    pathname: R,
                    search: y,
                    hash: v,
                    state: x,
                    key: A,
                    unstable_mask: E
                },
                navigationType: r
            };
        }, [
            h,
            p,
            y,
            v,
            x,
            A,
            r,
            E
        ]);
        return on(M != null, `<Router basename="${h}"> is not able to match the URL "${p}${y}${v}" because it does not start with the basename, so the <Router> won't render anything.`), M == null ? null : T.createElement(Ze.Provider, {
            value: m
        }, T.createElement(Fl.Provider, {
            children: a,
            value: M
        }));
    }
    function vT({ children: n, location: a }) {
        return iT(lf(n), a);
    }
    function lf(n, a = []) {
        let l = [];
        return T.Children.forEach(n, (r, u)=>{
            if (!T.isValidElement(r)) return;
            let d = [
                ...a,
                u
            ];
            if (r.type === T.Fragment) {
                l.push.apply(l, lf(r.props.children, d));
                return;
            }
            Ht(r.type === Mr, `[${typeof r.type == "string" ? r.type : r.type.name}] is not a <Route> component. All component children of <Routes> must be a <Route> or <React.Fragment>`), Ht(!r.props.index || !r.props.children, "An index route cannot have child routes.");
            let f = {
                id: r.props.id || d.join("-"),
                caseSensitive: r.props.caseSensitive,
                element: r.props.element,
                Component: r.props.Component,
                index: r.props.index,
                path: r.props.path,
                middleware: r.props.middleware,
                loader: r.props.loader,
                action: r.props.action,
                hydrateFallbackElement: r.props.hydrateFallbackElement,
                HydrateFallback: r.props.HydrateFallback,
                errorElement: r.props.errorElement,
                ErrorBoundary: r.props.ErrorBoundary,
                hasErrorBoundary: r.props.hasErrorBoundary === !0 || r.props.ErrorBoundary != null || r.props.errorElement != null,
                shouldRevalidate: r.props.shouldRevalidate,
                handle: r.props.handle,
                lazy: r.props.lazy
            };
            r.props.children && (f.children = lf(r.props.children, d)), l.push(f);
        }), l;
    }
    var Dr = "get", jr = "application/x-www-form-urlencoded";
    function Jr(n) {
        return typeof HTMLElement < "u" && n instanceof HTMLElement;
    }
    function bT(n) {
        return Jr(n) && n.tagName.toLowerCase() === "button";
    }
    function xT(n) {
        return Jr(n) && n.tagName.toLowerCase() === "form";
    }
    function ST(n) {
        return Jr(n) && n.tagName.toLowerCase() === "input";
    }
    function TT(n) {
        return !!(n.metaKey || n.altKey || n.ctrlKey || n.shiftKey);
    }
    function ET(n, a) {
        return n.button === 0 && (!a || a === "_self") && !TT(n);
    }
    var Tr = null;
    function AT() {
        if (Tr === null) try {
            new FormData(document.createElement("form"), 0), Tr = !1;
        } catch  {
            Tr = !0;
        }
        return Tr;
    }
    var CT = new Set([
        "application/x-www-form-urlencoded",
        "multipart/form-data",
        "text/plain"
    ]);
    function jc(n) {
        return n != null && !CT.has(n) ? (on(!1, `"${n}" is not a valid \`encType\` for \`<Form>\`/\`<fetcher.Form>\` and will default to "${jr}"`), null) : n;
    }
    function wT(n, a) {
        let l, r, u, d, f;
        if (xT(n)) {
            let h = n.getAttribute("action");
            r = h ? Mn(h, a) : null, l = n.getAttribute("method") || Dr, u = jc(n.getAttribute("enctype")) || jr, d = new FormData(n);
        } else if (bT(n) || ST(n) && (n.type === "submit" || n.type === "image")) {
            let h = n.form;
            if (h == null) throw new Error('Cannot submit a <button> or <input type="submit"> without a <form>');
            let m = n.getAttribute("formaction") || h.getAttribute("action");
            if (r = m ? Mn(m, a) : null, l = n.getAttribute("formmethod") || h.getAttribute("method") || Dr, u = jc(n.getAttribute("formenctype")) || jc(h.getAttribute("enctype")) || jr, d = new FormData(h, n), !AT()) {
                let { name: p, type: y, value: v } = n;
                if (y === "image") {
                    let x = p ? `${p}.` : "";
                    d.append(`${x}x`, "0"), d.append(`${x}y`, "0");
                } else p && d.append(p, v);
            }
        } else {
            if (Jr(n)) throw new Error('Cannot submit element that is not <form>, <button>, or <input type="submit|image">');
            l = Dr, r = null, u = jr, f = n;
        }
        return d && u === "text/plain" && (f = d, d = void 0), {
            action: r,
            method: l.toLowerCase(),
            encType: u,
            formData: d,
            body: f
        };
    }
    Object.getOwnPropertyNames(Object.prototype).sort().join("\0");
    function Uf(n, a) {
        if (n === !1 || n === null || typeof n > "u") throw new Error(a);
    }
    function _T(n, a, l, r) {
        let u = typeof n == "string" ? new URL(n, typeof window > "u" ? "server://singlefetch/" : window.location.origin) : n;
        return l ? u.pathname.endsWith("/") ? u.pathname = `${u.pathname}_.${r}` : u.pathname = `${u.pathname}.${r}` : u.pathname === "/" ? u.pathname = `_root.${r}` : a && Mn(u.pathname, a) === "/" ? u.pathname = `${a.replace(/\/$/, "")}/_root.${r}` : u.pathname = `${u.pathname.replace(/\/$/, "")}.${r}`, u;
    }
    async function RT(n, a) {
        if (n.id in a) return a[n.id];
        try {
            let l = await import(n.module).then(async (m)=>{
                await m.__tla;
                return m;
            });
            return a[n.id] = l, l;
        } catch (l) {
            return console.error(`Error loading route module \`${n.module}\`, reloading page...`), console.error(l), window.__reactRouterContext && window.__reactRouterContext.isSpaMode, window.location.reload(), new Promise(()=>{});
        }
    }
    function MT(n) {
        return n == null ? !1 : n.href == null ? n.rel === "preload" && typeof n.imageSrcSet == "string" && typeof n.imageSizes == "string" : typeof n.rel == "string" && typeof n.href == "string";
    }
    async function DT(n, a, l) {
        let r = await Promise.all(n.map(async (u)=>{
            let d = a.routes[u.route.id];
            if (d) {
                let f = await RT(d, l);
                return f.links ? f.links() : [];
            }
            return [];
        }));
        return zT(r.flat(1).filter(MT).filter((u)=>u.rel === "stylesheet" || u.rel === "preload").map((u)=>u.rel === "stylesheet" ? {
                ...u,
                rel: "prefetch",
                as: "style"
            } : {
                ...u,
                rel: "prefetch"
            }));
    }
    function Gg(n, a, l, r, u, d) {
        let f = (m, p)=>l[p] ? m.route.id !== l[p].route.id : !0, h = (m, p)=>l[p].pathname !== m.pathname || l[p].route.path?.endsWith("*") && l[p].params["*"] !== m.params["*"];
        return d === "assets" ? a.filter((m, p)=>f(m, p) || h(m, p)) : d === "data" ? a.filter((m, p)=>{
            let y = r.routes[m.route.id];
            if (!y || !y.hasLoader) return !1;
            if (f(m, p) || h(m, p)) return !0;
            if (m.route.shouldRevalidate) {
                let v = m.route.shouldRevalidate({
                    currentUrl: new URL(u.pathname + u.search + u.hash, window.origin),
                    currentParams: l[0]?.params || {},
                    nextUrl: new URL(n, window.origin),
                    nextParams: m.params,
                    defaultShouldRevalidate: !0
                });
                if (typeof v == "boolean") return v;
            }
            return !0;
        }) : [];
    }
    function jT(n, a, { includeHydrateFallback: l } = {}) {
        return OT(n.map((r)=>{
            let u = a.routes[r.route.id];
            if (!u) return [];
            let d = [
                u.module
            ];
            return u.clientActionModule && (d = d.concat(u.clientActionModule)), u.clientLoaderModule && (d = d.concat(u.clientLoaderModule)), l && u.hydrateFallbackModule && (d = d.concat(u.hydrateFallbackModule)), u.imports && (d = d.concat(u.imports)), d;
        }).flat(1));
    }
    function OT(n) {
        return [
            ...new Set(n)
        ];
    }
    function NT(n) {
        let a = {}, l = Object.keys(n).sort();
        for (let r of l)a[r] = n[r];
        return a;
    }
    function zT(n, a) {
        let l = new Set;
        return new Set(a), n.reduce((r, u)=>{
            let d = JSON.stringify(NT(u));
            return l.has(d) || (l.add(d), r.push({
                key: d,
                link: u
            })), r;
        }, []);
    }
    function z0() {
        let n = T.useContext(ji);
        return Uf(n, "You must render this element inside a <DataRouterContext.Provider> element"), n;
    }
    function LT() {
        let n = T.useContext($r);
        return Uf(n, "You must render this element inside a <DataRouterStateContext.Provider> element"), n;
    }
    var Hf = T.createContext(void 0);
    Hf.displayName = "FrameworkContext";
    function L0() {
        let n = T.useContext(Hf);
        return Uf(n, "You must render this element inside a <HydratedRouter> element"), n;
    }
    function VT(n, a) {
        let l = T.useContext(Hf), [r, u] = T.useState(!1), [d, f] = T.useState(!1), { onFocus: h, onBlur: m, onMouseEnter: p, onMouseLeave: y, onTouchStart: v } = a, x = T.useRef(null);
        T.useEffect(()=>{
            if (n === "render" && f(!0), n === "viewport") {
                let M = (z)=>{
                    z.forEach((B)=>{
                        f(B.isIntersecting);
                    });
                }, R = new IntersectionObserver(M, {
                    threshold: .5
                });
                return x.current && R.observe(x.current), ()=>{
                    R.disconnect();
                };
            }
        }, [
            n
        ]), T.useEffect(()=>{
            if (r) {
                let M = setTimeout(()=>{
                    f(!0);
                }, 100);
                return ()=>{
                    clearTimeout(M);
                };
            }
        }, [
            r
        ]);
        let A = ()=>{
            u(!0);
        }, E = ()=>{
            u(!1), f(!1);
        };
        return l ? n !== "intent" ? [
            d,
            x,
            {}
        ] : [
            d,
            x,
            {
                onFocus: Dl(h, A),
                onBlur: Dl(m, E),
                onMouseEnter: Dl(p, A),
                onMouseLeave: Dl(y, E),
                onTouchStart: Dl(v, A)
            }
        ] : [
            !1,
            x,
            {}
        ];
    }
    function Dl(n, a) {
        return (l)=>{
            n && n(l), l.defaultPrevented || a(l);
        };
    }
    function BT({ page: n, ...a }) {
        let { router: l } = z0(), r = T.useMemo(()=>x0(l.routes, n, l.basename), [
            l.routes,
            n,
            l.basename
        ]);
        return r ? T.createElement(HT, {
            page: n,
            matches: r,
            ...a
        }) : null;
    }
    function UT(n) {
        let { manifest: a, routeModules: l } = L0(), [r, u] = T.useState([]);
        return T.useEffect(()=>{
            let d = !1;
            return DT(n, a, l).then((f)=>{
                d || u(f);
            }), ()=>{
                d = !0;
            };
        }, [
            n,
            a,
            l
        ]), r;
    }
    function HT({ page: n, matches: a, ...l }) {
        let r = ua(), { future: u, manifest: d, routeModules: f } = L0(), { basename: h } = z0(), { loaderData: m, matches: p } = LT(), y = T.useMemo(()=>Gg(n, a, p, d, r, "data"), [
            n,
            a,
            p,
            d,
            r
        ]), v = T.useMemo(()=>Gg(n, a, p, d, r, "assets"), [
            n,
            a,
            p,
            d,
            r
        ]), x = T.useMemo(()=>{
            if (n === r.pathname + r.search + r.hash) return [];
            let M = new Set, R = !1;
            if (a.forEach((B)=>{
                let V = d.routes[B.route.id];
                !V || !V.hasLoader || (!y.some((P)=>P.route.id === B.route.id) && B.route.id in m && f[B.route.id]?.shouldRevalidate || V.hasClientLoader ? R = !0 : M.add(B.route.id));
            }), M.size === 0) return [];
            let z = _T(n, h, u.unstable_trailingSlashAwareDataRequests, "data");
            return R && M.size > 0 && z.searchParams.set("_routes", a.filter((B)=>M.has(B.route.id)).map((B)=>B.route.id).join(",")), [
                z.pathname + z.search
            ];
        }, [
            h,
            u.unstable_trailingSlashAwareDataRequests,
            m,
            r,
            d,
            y,
            a,
            n,
            f
        ]), A = T.useMemo(()=>jT(v, d), [
            v,
            d
        ]), E = UT(v);
        return T.createElement(T.Fragment, null, x.map((M)=>T.createElement("link", {
                key: M,
                rel: "prefetch",
                as: "fetch",
                href: M,
                ...l
            })), A.map((M)=>T.createElement("link", {
                key: M,
                rel: "modulepreload",
                href: M,
                ...l
            })), E.map(({ key: M, link: R })=>T.createElement("link", {
                key: M,
                nonce: l.nonce,
                ...R,
                crossOrigin: R.crossOrigin ?? l.crossOrigin
            })));
    }
    function GT(...n) {
        return (a)=>{
            n.forEach((l)=>{
                typeof l == "function" ? l(a) : l != null && (l.current = a);
            });
        };
    }
    var qT = typeof window < "u" && typeof window.document < "u" && typeof window.document.createElement < "u";
    try {
        qT && (window.__reactRouterVersion = "7.13.1");
    } catch  {}
    function kT({ basename: n, children: a, unstable_useTransitions: l, window: r }) {
        let u = T.useRef();
        u.current == null && (u.current = T1({
            window: r,
            v5Compat: !0
        }));
        let d = u.current, [f, h] = T.useState({
            action: d.action,
            location: d.location
        }), m = T.useCallback((p)=>{
            l === !1 ? h(p) : T.startTransition(()=>h(p));
        }, [
            l
        ]);
        return T.useLayoutEffect(()=>d.listen(m), [
            d,
            m
        ]), T.createElement(yT, {
            basename: n,
            children: a,
            location: f.location,
            navigationType: f.action,
            navigator: d,
            unstable_useTransitions: l
        });
    }
    var V0 = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i, B0 = T.forwardRef(function({ onClick: a, discover: l = "render", prefetch: r = "none", relative: u, reloadDocument: d, replace: f, unstable_mask: h, state: m, target: p, to: y, preventScrollReset: v, viewTransition: x, unstable_defaultShouldRevalidate: A, ...E }, M) {
        let { basename: R, navigator: z, unstable_useTransitions: B } = T.useContext(Ze), V = typeof y == "string" && V0.test(y), P = C0(y, R);
        y = P.to;
        let U = nT(y, {
            relative: u
        }), X = ua(), H = null;
        if (h) {
            let Vt = Nf(h, [], X.unstable_mask ? X.unstable_mask.pathname : "/", !0);
            R !== "/" && (Vt.pathname = Vt.pathname === "/" ? R : sn([
                R,
                Vt.pathname
            ])), H = z.createHref(Vt);
        }
        let [Z, Q, it] = VT(r, E), bt = PT(y, {
            replace: f,
            unstable_mask: h,
            state: m,
            target: p,
            preventScrollReset: v,
            relative: u,
            viewTransition: x,
            unstable_defaultShouldRevalidate: A,
            unstable_useTransitions: B
        });
        function gt(Vt) {
            a && a(Vt), Vt.defaultPrevented || bt(Vt);
        }
        let Nt = !(P.isExternal || d), ee = T.createElement("a", {
            ...E,
            ...it,
            href: (Nt ? H : void 0) || P.absoluteURL || U,
            onClick: Nt ? gt : a,
            ref: GT(M, Q),
            target: p,
            "data-discover": !V && l === "render" ? "true" : void 0
        });
        return Z && !V ? T.createElement(T.Fragment, null, ee, T.createElement(BT, {
            page: U
        })) : ee;
    });
    B0.displayName = "Link";
    var YT = T.forwardRef(function({ "aria-current": a = "page", caseSensitive: l = !1, className: r = "", end: u = !1, style: d, to: f, viewTransition: h, children: m, ...p }, y) {
        let v = Jl(f, {
            relative: p.relative
        }), x = ua(), A = T.useContext($r), { navigator: E, basename: M } = T.useContext(Ze), R = A != null && JT(v) && h === !0, z = E.encodeLocation ? E.encodeLocation(v).pathname : v.pathname, B = x.pathname, V = A && A.navigation && A.navigation.location ? A.navigation.location.pathname : null;
        l || (B = B.toLowerCase(), V = V ? V.toLowerCase() : null, z = z.toLowerCase()), V && M && (V = Mn(V, M) || V);
        const P = z !== "/" && z.endsWith("/") ? z.length - 1 : z.length;
        let U = B === z || !u && B.startsWith(z) && B.charAt(P) === "/", X = V != null && (V === z || !u && V.startsWith(z) && V.charAt(z.length) === "/"), H = {
            isActive: U,
            isPending: X,
            isTransitioning: R
        }, Z = U ? a : void 0, Q;
        typeof r == "function" ? Q = r(H) : Q = [
            r,
            U ? "active" : null,
            X ? "pending" : null,
            R ? "transitioning" : null
        ].filter(Boolean).join(" ");
        let it = typeof d == "function" ? d(H) : d;
        return T.createElement(B0, {
            ...p,
            "aria-current": Z,
            className: Q,
            ref: y,
            style: it,
            to: f,
            viewTransition: h
        }, typeof m == "function" ? m(H) : m);
    });
    YT.displayName = "NavLink";
    var XT = T.forwardRef(({ discover: n = "render", fetcherKey: a, navigate: l, reloadDocument: r, replace: u, state: d, method: f = Dr, action: h, onSubmit: m, relative: p, preventScrollReset: y, viewTransition: v, unstable_defaultShouldRevalidate: x, ...A }, E)=>{
        let { unstable_useTransitions: M } = T.useContext(Ze), R = FT(), z = $T(h, {
            relative: p
        }), B = f.toLowerCase() === "get" ? "get" : "post", V = typeof h == "string" && V0.test(h), P = (U)=>{
            if (m && m(U), U.defaultPrevented) return;
            U.preventDefault();
            let X = U.nativeEvent.submitter, H = X?.getAttribute("formmethod") || f, Z = ()=>R(X || U.currentTarget, {
                    fetcherKey: a,
                    method: H,
                    navigate: l,
                    replace: u,
                    state: d,
                    relative: p,
                    preventScrollReset: y,
                    viewTransition: v,
                    unstable_defaultShouldRevalidate: x
                });
            M && l !== !1 ? T.startTransition(()=>Z()) : Z();
        };
        return T.createElement("form", {
            ref: E,
            method: B,
            action: z,
            onSubmit: r ? m : P,
            ...A,
            "data-discover": !V && n === "render" ? "true" : void 0
        });
    });
    XT.displayName = "Form";
    function KT(n) {
        return `${n} must be used within a data router.  See https://reactrouter.com/en/main/routers/picking-a-router.`;
    }
    function U0(n) {
        let a = T.useContext(ji);
        return Ht(a, KT(n)), a;
    }
    function PT(n, { target: a, replace: l, unstable_mask: r, state: u, preventScrollReset: d, relative: f, viewTransition: h, unstable_defaultShouldRevalidate: m, unstable_useTransitions: p } = {}) {
        let y = Lf(), v = ua(), x = Jl(n, {
            relative: f
        });
        return T.useCallback((A)=>{
            if (ET(A, a)) {
                A.preventDefault();
                let E = l !== void 0 ? l : kl(v) === kl(x), M = ()=>y(n, {
                        replace: E,
                        unstable_mask: r,
                        state: u,
                        preventScrollReset: d,
                        relative: f,
                        viewTransition: h,
                        unstable_defaultShouldRevalidate: m
                    });
                p ? T.startTransition(()=>M()) : M();
            }
        }, [
            v,
            y,
            x,
            l,
            r,
            u,
            a,
            n,
            d,
            f,
            h,
            m,
            p
        ]);
    }
    var ZT = 0, QT = ()=>`__${String(++ZT)}__`;
    function FT() {
        let { router: n } = U0("useSubmit"), { basename: a } = T.useContext(Ze), l = hT(), r = n.fetch, u = n.navigate;
        return T.useCallback(async (d, f = {})=>{
            let { action: h, method: m, encType: p, formData: y, body: v } = wT(d, a);
            if (f.navigate === !1) {
                let x = f.fetcherKey || QT();
                await r(x, l, f.action || h, {
                    unstable_defaultShouldRevalidate: f.unstable_defaultShouldRevalidate,
                    preventScrollReset: f.preventScrollReset,
                    formData: y,
                    body: v,
                    formMethod: f.method || m,
                    formEncType: f.encType || p,
                    flushSync: f.flushSync
                });
            } else await u(f.action || h, {
                unstable_defaultShouldRevalidate: f.unstable_defaultShouldRevalidate,
                preventScrollReset: f.preventScrollReset,
                formData: y,
                body: v,
                formMethod: f.method || m,
                formEncType: f.encType || p,
                replace: f.replace,
                state: f.state,
                fromRouteId: l,
                flushSync: f.flushSync,
                viewTransition: f.viewTransition
            });
        }, [
            r,
            u,
            a,
            l
        ]);
    }
    function $T(n, { relative: a } = {}) {
        let { basename: l } = T.useContext(Ze), r = T.useContext(On);
        Ht(r, "useFormAction must be used inside a RouteContext");
        let [u] = r.matches.slice(-1), d = {
            ...Jl(n || ".", {
                relative: a
            })
        }, f = ua();
        if (n == null) {
            d.search = f.search;
            let h = new URLSearchParams(d.search), m = h.getAll("index");
            if (m.some((y)=>y === "")) {
                h.delete("index"), m.filter((v)=>v).forEach((v)=>h.append("index", v));
                let y = h.toString();
                d.search = y ? `?${y}` : "";
            }
        }
        return (!n || n === ".") && u.route.index && (d.search = d.search ? d.search.replace(/^\?/, "?index&") : "?index"), l !== "/" && (d.pathname = d.pathname === "/" ? l : sn([
            l,
            d.pathname
        ])), kl(d);
    }
    function JT(n, { relative: a } = {}) {
        let l = T.useContext(_0);
        Ht(l != null, "`useViewTransitionState` must be used within `react-router-dom`'s `RouterProvider`.  Did you accidentally import `RouterProvider` from `react-router`?");
        let { basename: r } = U0("useViewTransitionState"), u = Jl(n, {
            relative: a
        });
        if (!l.isTransitioning) return !1;
        let d = Mn(l.currentLocation.pathname, r) || l.currentLocation.pathname, f = Mn(l.nextLocation.pathname, r) || l.nextLocation.pathname;
        return qr(u.pathname, f) != null || qr(u.pathname, d) != null;
    }
    const Oc = [
        "DealDamage",
        "GainLife",
        "LoseLife",
        "DrawCard",
        "Discard",
        "DestroyTarget",
        "DestroyAll",
        "ExileTarget",
        "BounceTarget",
        "CounterSpell",
        "PutCounter",
        "RemoveCounter",
        "CreateToken",
        "PumpTarget",
        "PumpAll",
        "TapTarget",
        "UntapTarget",
        "GainControl",
        "Sacrifice",
        "Mill",
        "Scry",
        "Surveil",
        "FightTarget",
        "AttachTarget",
        "Search",
        "Regenerate",
        "PreventDamage",
        "SetPowerToughness",
        "SwitchPowerToughness",
        "CopySpell",
        "Proliferate",
        "Transform",
        "Manifest",
        "PhaseOut",
        "Populate",
        "Amass",
        "Adapt",
        "Explore"
    ], Nc = [
        "ChangesZone",
        "Attacks",
        "Blocks",
        "BecomesTarget",
        "SpellCast",
        "AbilityActivated",
        "Damaged",
        "Untaps",
        "TurnBegin",
        "PhaseBegin",
        "Draws",
        "Discards",
        "LandPlayed",
        "LifeGained",
        "LifeLost",
        "CounterAdded",
        "CounterRemoved",
        "TokenCreated",
        "Dies",
        "Sacrificed",
        "Destroyed",
        "Exiled",
        "Returned",
        "Attached",
        "Detached",
        "Transformed",
        "TappedForMana"
    ], zc = [
        "Flying",
        "First Strike",
        "Double Strike",
        "Deathtouch",
        "Haste",
        "Hexproof",
        "Indestructible",
        "Lifelink",
        "Menace",
        "Reach",
        "Trample",
        "Vigilance",
        "Flash",
        "Defender",
        "Fear",
        "Intimidate",
        "Shroud",
        "Protection",
        "Ward",
        "Prowess",
        "Convoke",
        "Delve",
        "Cascade",
        "Cycling",
        "Equip",
        "Enchant",
        "Kicker",
        "Flashback",
        "Retrace",
        "Unearth",
        "Bestow",
        "Morph",
        "Emerge",
        "Evoke",
        "Suspend",
        "Madness",
        "Miracle",
        "Overload",
        "Entwine",
        "Buyback",
        "Affinity",
        "Metalcraft",
        "Landfall",
        "Revolt",
        "Ferocious",
        "Heroic",
        "Constellation",
        "Devotion",
        "Crew",
        "Fabricate",
        "Exploit",
        "Outlast"
    ];
    function WT({ onClose: n }) {
        const [a, l] = T.useState(""), [r, u] = T.useState("effects"), d = T.useMemo(()=>{
            const h = a.toLowerCase(), m = r === "effects" ? Oc : r === "triggers" ? Nc : zc;
            return h ? m.filter((p)=>p.toLowerCase().includes(h)) : m;
        }, [
            a,
            r
        ]), f = [
            {
                key: "effects",
                label: "Effects",
                count: Oc.length
            },
            {
                key: "triggers",
                label: "Triggers",
                count: Nc.length
            },
            {
                key: "keywords",
                label: "Keywords",
                count: zc.length
            }
        ];
        return S.jsxs("div", {
            className: "fixed inset-0 z-50 flex items-center justify-center",
            children: [
                S.jsx("div", {
                    className: "absolute inset-0 bg-black/60",
                    onClick: n
                }),
                S.jsxs("div", {
                    className: "relative z-10 flex max-h-[80vh] w-full max-w-2xl flex-col rounded-xl bg-gray-900 shadow-2xl ring-1 ring-gray-700",
                    children: [
                        S.jsxs("div", {
                            className: "flex items-center justify-between border-b border-gray-800 p-4",
                            children: [
                                S.jsx("h2", {
                                    className: "text-lg font-bold text-white",
                                    children: "Card Coverage Dashboard"
                                }),
                                S.jsx("button", {
                                    onClick: n,
                                    className: "rounded-lg p-1 text-gray-400 transition hover:bg-gray-800 hover:text-white",
                                    children: S.jsx("span", {
                                        className: "text-xl leading-none",
                                        children: "×"
                                    })
                                })
                            ]
                        }),
                        S.jsx("div", {
                            className: "flex gap-4 border-b border-gray-800 px-4 py-3",
                            children: f.map((h)=>S.jsxs("button", {
                                    onClick: ()=>u(h.key),
                                    className: `rounded-lg px-3 py-1.5 text-sm font-medium transition ${r === h.key ? "bg-indigo-600 text-white" : "text-gray-400 hover:bg-gray-800 hover:text-white"}`,
                                    children: [
                                        h.label,
                                        " (",
                                        h.count,
                                        ")"
                                    ]
                                }, h.key))
                        }),
                        S.jsx("div", {
                            className: "border-b border-gray-800 px-4 py-2",
                            children: S.jsx("input", {
                                type: "text",
                                placeholder: "Search...",
                                value: a,
                                onChange: (h)=>l(h.target.value),
                                className: "w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-indigo-500"
                            })
                        }),
                        S.jsxs("div", {
                            className: "flex-1 overflow-y-auto p-4",
                            children: [
                                S.jsx("div", {
                                    className: "grid grid-cols-2 gap-1 sm:grid-cols-3",
                                    children: d.map((h)=>S.jsxs("div", {
                                            className: "flex items-center gap-2 rounded px-2 py-1 text-sm text-gray-300",
                                            children: [
                                                S.jsx("span", {
                                                    className: "text-emerald-400",
                                                    children: "✓"
                                                }),
                                                h
                                            ]
                                        }, h))
                                }),
                                d.length === 0 && S.jsxs("p", {
                                    className: "py-8 text-center text-sm text-gray-500",
                                    children: [
                                        "No matches found for “",
                                        a,
                                        "”"
                                    ]
                                })
                            ]
                        }),
                        S.jsxs("div", {
                            className: "border-t border-gray-800 px-4 py-3 text-center text-xs text-gray-500",
                            children: [
                                Oc.length,
                                " effect types | ",
                                Nc.length,
                                " trigger modes | ",
                                zc.length,
                                " keywords"
                            ]
                        })
                    ]
                })
            ]
        });
    }
    function IT() {
        const n = Lf(), [a, l] = T.useState(!1);
        return S.jsxs("div", {
            className: "flex min-h-screen flex-col items-center justify-center gap-8",
            children: [
                S.jsx("h1", {
                    className: "text-5xl font-bold tracking-tight",
                    children: "Forge.ts"
                }),
                S.jsx("p", {
                    className: "text-gray-400",
                    children: "Magic: The Gathering Engine"
                }),
                S.jsxs("div", {
                    className: "flex flex-col gap-4",
                    children: [
                        S.jsx("button", {
                            onClick: ()=>n("/game"),
                            className: "rounded-lg bg-indigo-600 px-8 py-3 text-lg font-semibold transition-colors hover:bg-indigo-500",
                            children: "New Game"
                        }),
                        S.jsx("button", {
                            onClick: ()=>n("/deck-builder"),
                            className: "rounded-lg border border-gray-600 px-8 py-3 text-lg font-semibold transition-colors hover:border-gray-400",
                            children: "Deck Builder"
                        }),
                        S.jsx("button", {
                            onClick: ()=>l(!0),
                            className: "rounded-lg border border-gray-600 px-8 py-3 text-lg font-semibold transition-colors hover:border-gray-400",
                            children: "Card Coverage"
                        })
                    ]
                }),
                a && S.jsx(WT, {
                    onClose: ()=>l(!1)
                })
            ]
        });
    }
    const Gf = T.createContext({});
    function Wl(n) {
        const a = T.useRef(null);
        return a.current === null && (a.current = n()), a.current;
    }
    const tE = typeof window < "u", qf = tE ? T.useLayoutEffect : T.useEffect, Wr = T.createContext(null);
    function kf(n, a) {
        n.indexOf(a) === -1 && n.push(a);
    }
    function Ri(n, a) {
        const l = n.indexOf(a);
        l > -1 && n.splice(l, 1);
    }
    const un = (n, a, l)=>l > a ? a : l < n ? n : l;
    let Yf = ()=>{};
    const Dn = {}, H0 = (n)=>/^-?(?:\d+(?:\.\d+)?|\.\d+)$/u.test(n);
    function G0(n) {
        return typeof n == "object" && n !== null;
    }
    const q0 = (n)=>/^0[^.\s]+$/u.test(n);
    function k0(n) {
        let a;
        return ()=>(a === void 0 && (a = n()), a);
    }
    const Ke = (n)=>n, eE = (n, a)=>(l)=>a(n(l)), Il = (...n)=>n.reduce(eE), Mi = (n, a, l)=>{
        const r = a - n;
        return r === 0 ? 1 : (l - n) / r;
    };
    class Xf {
        constructor(){
            this.subscriptions = [];
        }
        add(a) {
            return kf(this.subscriptions, a), ()=>Ri(this.subscriptions, a);
        }
        notify(a, l, r) {
            const u = this.subscriptions.length;
            if (u) if (u === 1) this.subscriptions[0](a, l, r);
            else for(let d = 0; d < u; d++){
                const f = this.subscriptions[d];
                f && f(a, l, r);
            }
        }
        getSize() {
            return this.subscriptions.length;
        }
        clear() {
            this.subscriptions.length = 0;
        }
    }
    const Pe = (n)=>n * 1e3, Xe = (n)=>n / 1e3;
    function Y0(n, a) {
        return a ? n * (1e3 / a) : 0;
    }
    const nE = (n, a, l)=>{
        const r = a - n;
        return ((l - n) % r + r) % r + n;
    }, X0 = (n, a, l)=>(((1 - 3 * l + 3 * a) * n + (3 * l - 6 * a)) * n + 3 * a) * n, aE = 1e-7, iE = 12;
    function lE(n, a, l, r, u) {
        let d, f, h = 0;
        do f = a + (l - a) / 2, d = X0(f, r, u) - n, d > 0 ? l = f : a = f;
        while (Math.abs(d) > aE && ++h < iE);
        return f;
    }
    function ts(n, a, l, r) {
        if (n === a && l === r) return Ke;
        const u = (d)=>lE(d, 0, 1, n, l);
        return (d)=>d === 0 || d === 1 ? d : X0(u(d), a, r);
    }
    const K0 = (n)=>(a)=>a <= .5 ? n(2 * a) / 2 : (2 - n(2 * (1 - a))) / 2, P0 = (n)=>(a)=>1 - n(1 - a), Z0 = ts(.33, 1.53, .69, .99), Kf = P0(Z0), Q0 = K0(Kf), F0 = (n)=>(n *= 2) < 1 ? .5 * Kf(n) : .5 * (2 - Math.pow(2, -10 * (n - 1))), Pf = (n)=>1 - Math.sin(Math.acos(n)), $0 = P0(Pf), J0 = K0(Pf), sE = ts(.42, 0, 1, 1), rE = ts(0, 0, .58, 1), W0 = ts(.42, 0, .58, 1), I0 = (n)=>Array.isArray(n) && typeof n[0] != "number";
    function tv(n, a) {
        return I0(n) ? n[nE(0, n.length, a)] : n;
    }
    const ev = (n)=>Array.isArray(n) && typeof n[0] == "number", oE = {
        linear: Ke,
        easeIn: sE,
        easeInOut: W0,
        easeOut: rE,
        circIn: Pf,
        circInOut: J0,
        circOut: $0,
        backIn: Kf,
        backInOut: Q0,
        backOut: Z0,
        anticipate: F0
    }, uE = (n)=>typeof n == "string", qg = (n)=>{
        if (ev(n)) {
            Yf(n.length === 4);
            const [a, l, r, u] = n;
            return ts(a, l, r, u);
        } else if (uE(n)) return oE[n];
        return n;
    }, Er = [
        "setup",
        "read",
        "resolveKeyframes",
        "preUpdate",
        "update",
        "preRender",
        "render",
        "postRender"
    ];
    function cE(n, a) {
        let l = new Set, r = new Set, u = !1, d = !1;
        const f = new WeakSet;
        let h = {
            delta: 0,
            timestamp: 0,
            isProcessing: !1
        };
        function m(y) {
            f.has(y) && (p.schedule(y), n()), y(h);
        }
        const p = {
            schedule: (y, v = !1, x = !1)=>{
                const E = x && u ? l : r;
                return v && f.add(y), E.has(y) || E.add(y), y;
            },
            cancel: (y)=>{
                r.delete(y), f.delete(y);
            },
            process: (y)=>{
                if (h = y, u) {
                    d = !0;
                    return;
                }
                u = !0, [l, r] = [
                    r,
                    l
                ], l.forEach(m), l.clear(), u = !1, d && (d = !1, p.process(y));
            }
        };
        return p;
    }
    const fE = 40;
    function nv(n, a) {
        let l = !1, r = !0;
        const u = {
            delta: 0,
            timestamp: 0,
            isProcessing: !1
        }, d = ()=>l = !0, f = Er.reduce((V, P)=>(V[P] = cE(d), V), {}), { setup: h, read: m, resolveKeyframes: p, preUpdate: y, update: v, preRender: x, render: A, postRender: E } = f, M = ()=>{
            const V = Dn.useManualTiming ? u.timestamp : performance.now();
            l = !1, Dn.useManualTiming || (u.delta = r ? 1e3 / 60 : Math.max(Math.min(V - u.timestamp, fE), 1)), u.timestamp = V, u.isProcessing = !0, h.process(u), m.process(u), p.process(u), y.process(u), v.process(u), x.process(u), A.process(u), E.process(u), u.isProcessing = !1, l && a && (r = !1, n(M));
        }, R = ()=>{
            l = !0, r = !0, u.isProcessing || n(M);
        };
        return {
            schedule: Er.reduce((V, P)=>{
                const U = f[P];
                return V[P] = (X, H = !1, Z = !1)=>(l || R(), U.schedule(X, H, Z)), V;
            }, {}),
            cancel: (V)=>{
                for(let P = 0; P < Er.length; P++)f[Er[P]].cancel(V);
            },
            state: u,
            steps: f
        };
    }
    const { schedule: Ot, cancel: jn, state: ce, steps: Lc } = nv(typeof requestAnimationFrame < "u" ? requestAnimationFrame : Ke, !0);
    let Or;
    function dE() {
        Or = void 0;
    }
    const me = {
        now: ()=>(Or === void 0 && me.set(ce.isProcessing || Dn.useManualTiming ? ce.timestamp : performance.now()), Or),
        set: (n)=>{
            Or = n, queueMicrotask(dE);
        }
    }, av = (n)=>(a)=>typeof a == "string" && a.startsWith(n), iv = av("--"), hE = av("var(--"), Zf = (n)=>hE(n) ? mE.test(n.split("/*")[0].trim()) : !1, mE = /var\(--(?:[\w-]+\s*|[\w-]+\s*,(?:\s*[^)(\s]|\s*\((?:[^)(]|\([^)(]*\))*\))+\s*)\)$/iu;
    function kg(n) {
        return typeof n != "string" ? !1 : n.split("/*")[0].includes("var(--");
    }
    const Oi = {
        test: (n)=>typeof n == "number",
        parse: parseFloat,
        transform: (n)=>n
    }, Yl = {
        ...Oi,
        transform: (n)=>un(0, 1, n)
    }, Ar = {
        ...Oi,
        default: 1
    }, Ll = (n)=>Math.round(n * 1e5) / 1e5, Qf = /-?(?:\d+(?:\.\d+)?|\.\d+)/gu;
    function pE(n) {
        return n == null;
    }
    const gE = /^(?:#[\da-f]{3,8}|(?:rgb|hsl)a?\((?:-?[\d.]+%?[,\s]+){2}-?[\d.]+%?\s*(?:[,/]\s*)?(?:\b\d+(?:\.\d+)?|\.\d+)?%?\))$/iu, Ff = (n, a)=>(l)=>!!(typeof l == "string" && gE.test(l) && l.startsWith(n) || a && !pE(l) && Object.prototype.hasOwnProperty.call(l, a)), lv = (n, a, l)=>(r)=>{
            if (typeof r != "string") return r;
            const [u, d, f, h] = r.match(Qf);
            return {
                [n]: parseFloat(u),
                [a]: parseFloat(d),
                [l]: parseFloat(f),
                alpha: h !== void 0 ? parseFloat(h) : 1
            };
        }, yE = (n)=>un(0, 255, n), Vc = {
        ...Oi,
        transform: (n)=>Math.round(yE(n))
    }, Na = {
        test: Ff("rgb", "red"),
        parse: lv("red", "green", "blue"),
        transform: ({ red: n, green: a, blue: l, alpha: r = 1 })=>"rgba(" + Vc.transform(n) + ", " + Vc.transform(a) + ", " + Vc.transform(l) + ", " + Ll(Yl.transform(r)) + ")"
    };
    function vE(n) {
        let a = "", l = "", r = "", u = "";
        return n.length > 5 ? (a = n.substring(1, 3), l = n.substring(3, 5), r = n.substring(5, 7), u = n.substring(7, 9)) : (a = n.substring(1, 2), l = n.substring(2, 3), r = n.substring(3, 4), u = n.substring(4, 5), a += a, l += l, r += r, u += u), {
            red: parseInt(a, 16),
            green: parseInt(l, 16),
            blue: parseInt(r, 16),
            alpha: u ? parseInt(u, 16) / 255 : 1
        };
    }
    const sf = {
        test: Ff("#"),
        parse: vE,
        transform: Na.transform
    }, es = (n)=>({
            test: (a)=>typeof a == "string" && a.endsWith(n) && a.split(" ").length === 1,
            parse: parseFloat,
            transform: (a)=>`${a}${n}`
        }), sa = es("deg"), rn = es("%"), tt = es("px"), bE = es("vh"), xE = es("vw"), Yg = {
        ...rn,
        parse: (n)=>rn.parse(n) / 100,
        transform: (n)=>rn.transform(n * 100)
    }, Ti = {
        test: Ff("hsl", "hue"),
        parse: lv("hue", "saturation", "lightness"),
        transform: ({ hue: n, saturation: a, lightness: l, alpha: r = 1 })=>"hsla(" + Math.round(n) + ", " + rn.transform(Ll(a)) + ", " + rn.transform(Ll(l)) + ", " + Ll(Yl.transform(r)) + ")"
    }, Wt = {
        test: (n)=>Na.test(n) || sf.test(n) || Ti.test(n),
        parse: (n)=>Na.test(n) ? Na.parse(n) : Ti.test(n) ? Ti.parse(n) : sf.parse(n),
        transform: (n)=>typeof n == "string" ? n : n.hasOwnProperty("red") ? Na.transform(n) : Ti.transform(n),
        getAnimatableNone: (n)=>{
            const a = Wt.parse(n);
            return a.alpha = 0, Wt.transform(a);
        }
    }, SE = /(?:#[\da-f]{3,8}|(?:rgb|hsl)a?\((?:-?[\d.]+%?[,\s]+){2}-?[\d.]+%?\s*(?:[,/]\s*)?(?:\b\d+(?:\.\d+)?|\.\d+)?%?\))/giu;
    function TE(n) {
        return isNaN(n) && typeof n == "string" && (n.match(Qf)?.length || 0) + (n.match(SE)?.length || 0) > 0;
    }
    const sv = "number", rv = "color", EE = "var", AE = "var(", Xg = "${}", CE = /var\s*\(\s*--(?:[\w-]+\s*|[\w-]+\s*,(?:\s*[^)(\s]|\s*\((?:[^)(]|\([^)(]*\))*\))+\s*)\)|#[\da-f]{3,8}|(?:rgb|hsl)a?\((?:-?[\d.]+%?[,\s]+){2}-?[\d.]+%?\s*(?:[,/]\s*)?(?:\b\d+(?:\.\d+)?|\.\d+)?%?\)|-?(?:\d+(?:\.\d+)?|\.\d+)/giu;
    function Xl(n) {
        const a = n.toString(), l = [], r = {
            color: [],
            number: [],
            var: []
        }, u = [];
        let d = 0;
        const h = a.replace(CE, (m)=>(Wt.test(m) ? (r.color.push(d), u.push(rv), l.push(Wt.parse(m))) : m.startsWith(AE) ? (r.var.push(d), u.push(EE), l.push(m)) : (r.number.push(d), u.push(sv), l.push(parseFloat(m))), ++d, Xg)).split(Xg);
        return {
            values: l,
            split: h,
            indexes: r,
            types: u
        };
    }
    function ov(n) {
        return Xl(n).values;
    }
    function uv(n) {
        const { split: a, types: l } = Xl(n), r = a.length;
        return (u)=>{
            let d = "";
            for(let f = 0; f < r; f++)if (d += a[f], u[f] !== void 0) {
                const h = l[f];
                h === sv ? d += Ll(u[f]) : h === rv ? d += Wt.transform(u[f]) : d += u[f];
            }
            return d;
        };
    }
    const wE = (n)=>typeof n == "number" ? 0 : Wt.test(n) ? Wt.getAnimatableNone(n) : n;
    function _E(n) {
        const a = ov(n);
        return uv(n)(a.map(wE));
    }
    const Ie = {
        test: TE,
        parse: ov,
        createTransformer: uv,
        getAnimatableNone: _E
    };
    function Bc(n, a, l) {
        return l < 0 && (l += 1), l > 1 && (l -= 1), l < 1 / 6 ? n + (a - n) * 6 * l : l < 1 / 2 ? a : l < 2 / 3 ? n + (a - n) * (2 / 3 - l) * 6 : n;
    }
    function RE({ hue: n, saturation: a, lightness: l, alpha: r }) {
        n /= 360, a /= 100, l /= 100;
        let u = 0, d = 0, f = 0;
        if (!a) u = d = f = l;
        else {
            const h = l < .5 ? l * (1 + a) : l + a - l * a, m = 2 * l - h;
            u = Bc(m, h, n + 1 / 3), d = Bc(m, h, n), f = Bc(m, h, n - 1 / 3);
        }
        return {
            red: Math.round(u * 255),
            green: Math.round(d * 255),
            blue: Math.round(f * 255),
            alpha: r
        };
    }
    function kr(n, a) {
        return (l)=>l > 0 ? a : n;
    }
    const Ut = (n, a, l)=>n + (a - n) * l, Uc = (n, a, l)=>{
        const r = n * n, u = l * (a * a - r) + r;
        return u < 0 ? 0 : Math.sqrt(u);
    }, ME = [
        sf,
        Na,
        Ti
    ], DE = (n)=>ME.find((a)=>a.test(n));
    function Kg(n) {
        const a = DE(n);
        if (!a) return !1;
        let l = a.parse(n);
        return a === Ti && (l = RE(l)), l;
    }
    const Pg = (n, a)=>{
        const l = Kg(n), r = Kg(a);
        if (!l || !r) return kr(n, a);
        const u = {
            ...l
        };
        return (d)=>(u.red = Uc(l.red, r.red, d), u.green = Uc(l.green, r.green, d), u.blue = Uc(l.blue, r.blue, d), u.alpha = Ut(l.alpha, r.alpha, d), Na.transform(u));
    }, rf = new Set([
        "none",
        "hidden"
    ]);
    function jE(n, a) {
        return rf.has(n) ? (l)=>l <= 0 ? n : a : (l)=>l >= 1 ? a : n;
    }
    function OE(n, a) {
        return (l)=>Ut(n, a, l);
    }
    function $f(n) {
        return typeof n == "number" ? OE : typeof n == "string" ? Zf(n) ? kr : Wt.test(n) ? Pg : LE : Array.isArray(n) ? cv : typeof n == "object" ? Wt.test(n) ? Pg : NE : kr;
    }
    function cv(n, a) {
        const l = [
            ...n
        ], r = l.length, u = n.map((d, f)=>$f(d)(d, a[f]));
        return (d)=>{
            for(let f = 0; f < r; f++)l[f] = u[f](d);
            return l;
        };
    }
    function NE(n, a) {
        const l = {
            ...n,
            ...a
        }, r = {};
        for(const u in l)n[u] !== void 0 && a[u] !== void 0 && (r[u] = $f(n[u])(n[u], a[u]));
        return (u)=>{
            for(const d in r)l[d] = r[d](u);
            return l;
        };
    }
    function zE(n, a) {
        const l = [], r = {
            color: 0,
            var: 0,
            number: 0
        };
        for(let u = 0; u < a.values.length; u++){
            const d = a.types[u], f = n.indexes[d][r[d]], h = n.values[f] ?? 0;
            l[u] = h, r[d]++;
        }
        return l;
    }
    const LE = (n, a)=>{
        const l = Ie.createTransformer(a), r = Xl(n), u = Xl(a);
        return r.indexes.var.length === u.indexes.var.length && r.indexes.color.length === u.indexes.color.length && r.indexes.number.length >= u.indexes.number.length ? rf.has(n) && !u.values.length || rf.has(a) && !r.values.length ? jE(n, a) : Il(cv(zE(r, u), u.values), l) : kr(n, a);
    };
    function fv(n, a, l) {
        return typeof n == "number" && typeof a == "number" && typeof l == "number" ? Ut(n, a, l) : $f(n)(n, a);
    }
    const VE = (n)=>{
        const a = ({ timestamp: l })=>n(l);
        return {
            start: (l = !0)=>Ot.update(a, l),
            stop: ()=>jn(a),
            now: ()=>ce.isProcessing ? ce.timestamp : me.now()
        };
    }, dv = (n, a, l = 10)=>{
        let r = "";
        const u = Math.max(Math.round(a / l), 2);
        for(let d = 0; d < u; d++)r += Math.round(n(d / (u - 1)) * 1e4) / 1e4 + ", ";
        return `linear(${r.substring(0, r.length - 2)})`;
    }, Yr = 2e4;
    function Jf(n) {
        let a = 0;
        const l = 50;
        let r = n.next(a);
        for(; !r.done && a < Yr;)a += l, r = n.next(a);
        return a >= Yr ? 1 / 0 : a;
    }
    function hv(n, a = 100, l) {
        const r = l({
            ...n,
            keyframes: [
                0,
                a
            ]
        }), u = Math.min(Jf(r), Yr);
        return {
            type: "keyframes",
            ease: (d)=>r.next(u * d).value / a,
            duration: Xe(u)
        };
    }
    const BE = 5;
    function mv(n, a, l) {
        const r = Math.max(a - BE, 0);
        return Y0(l - n(r), a - r);
    }
    const kt = {
        stiffness: 100,
        damping: 10,
        mass: 1,
        velocity: 0,
        duration: 800,
        bounce: .3,
        visualDuration: .3,
        restSpeed: {
            granular: .01,
            default: 2
        },
        restDelta: {
            granular: .005,
            default: .5
        },
        minDuration: .01,
        maxDuration: 10,
        minDamping: .05,
        maxDamping: 1
    }, Hc = .001;
    function UE({ duration: n = kt.duration, bounce: a = kt.bounce, velocity: l = kt.velocity, mass: r = kt.mass }) {
        let u, d, f = 1 - a;
        f = un(kt.minDamping, kt.maxDamping, f), n = un(kt.minDuration, kt.maxDuration, Xe(n)), f < 1 ? (u = (p)=>{
            const y = p * f, v = y * n, x = y - l, A = of(p, f), E = Math.exp(-v);
            return Hc - x / A * E;
        }, d = (p)=>{
            const v = p * f * n, x = v * l + l, A = Math.pow(f, 2) * Math.pow(p, 2) * n, E = Math.exp(-v), M = of(Math.pow(p, 2), f);
            return (-u(p) + Hc > 0 ? -1 : 1) * ((x - A) * E) / M;
        }) : (u = (p)=>{
            const y = Math.exp(-p * n), v = (p - l) * n + 1;
            return -Hc + y * v;
        }, d = (p)=>{
            const y = Math.exp(-p * n), v = (l - p) * (n * n);
            return y * v;
        });
        const h = 5 / n, m = GE(u, d, h);
        if (n = Pe(n), isNaN(m)) return {
            stiffness: kt.stiffness,
            damping: kt.damping,
            duration: n
        };
        {
            const p = Math.pow(m, 2) * r;
            return {
                stiffness: p,
                damping: f * 2 * Math.sqrt(r * p),
                duration: n
            };
        }
    }
    const HE = 12;
    function GE(n, a, l) {
        let r = l;
        for(let u = 1; u < HE; u++)r = r - n(r) / a(r);
        return r;
    }
    function of(n, a) {
        return n * Math.sqrt(1 - a * a);
    }
    const qE = [
        "duration",
        "bounce"
    ], kE = [
        "stiffness",
        "damping",
        "mass"
    ];
    function Zg(n, a) {
        return a.some((l)=>n[l] !== void 0);
    }
    function YE(n) {
        let a = {
            velocity: kt.velocity,
            stiffness: kt.stiffness,
            damping: kt.damping,
            mass: kt.mass,
            isResolvedFromDuration: !1,
            ...n
        };
        if (!Zg(n, kE) && Zg(n, qE)) if (a.velocity = 0, n.visualDuration) {
            const l = n.visualDuration, r = 2 * Math.PI / (l * 1.2), u = r * r, d = 2 * un(.05, 1, 1 - (n.bounce || 0)) * Math.sqrt(u);
            a = {
                ...a,
                mass: kt.mass,
                stiffness: u,
                damping: d
            };
        } else {
            const l = UE({
                ...n,
                velocity: 0
            });
            a = {
                ...a,
                ...l,
                mass: kt.mass
            }, a.isResolvedFromDuration = !0;
        }
        return a;
    }
    function Kl(n = kt.visualDuration, a = kt.bounce) {
        const l = typeof n != "object" ? {
            visualDuration: n,
            keyframes: [
                0,
                1
            ],
            bounce: a
        } : n;
        let { restSpeed: r, restDelta: u } = l;
        const d = l.keyframes[0], f = l.keyframes[l.keyframes.length - 1], h = {
            done: !1,
            value: d
        }, { stiffness: m, damping: p, mass: y, duration: v, velocity: x, isResolvedFromDuration: A } = YE({
            ...l,
            velocity: -Xe(l.velocity || 0)
        }), E = x || 0, M = p / (2 * Math.sqrt(m * y)), R = f - d, z = Xe(Math.sqrt(m / y)), B = Math.abs(R) < 5;
        r || (r = B ? kt.restSpeed.granular : kt.restSpeed.default), u || (u = B ? kt.restDelta.granular : kt.restDelta.default);
        let V;
        if (M < 1) {
            const U = of(z, M);
            V = (X)=>{
                const H = Math.exp(-M * z * X);
                return f - H * ((E + M * z * R) / U * Math.sin(U * X) + R * Math.cos(U * X));
            };
        } else if (M === 1) V = (U)=>f - Math.exp(-z * U) * (R + (E + z * R) * U);
        else {
            const U = z * Math.sqrt(M * M - 1);
            V = (X)=>{
                const H = Math.exp(-M * z * X), Z = Math.min(U * X, 300);
                return f - H * ((E + M * z * R) * Math.sinh(Z) + U * R * Math.cosh(Z)) / U;
            };
        }
        const P = {
            calculatedDuration: A && v || null,
            next: (U)=>{
                const X = V(U);
                if (A) h.done = U >= v;
                else {
                    let H = U === 0 ? E : 0;
                    M < 1 && (H = U === 0 ? Pe(E) : mv(V, U, X));
                    const Z = Math.abs(H) <= r, Q = Math.abs(f - X) <= u;
                    h.done = Z && Q;
                }
                return h.value = h.done ? f : X, h;
            },
            toString: ()=>{
                const U = Math.min(Jf(P), Yr), X = dv((H)=>P.next(U * H).value, U, 30);
                return U + "ms " + X;
            },
            toTransition: ()=>{}
        };
        return P;
    }
    Kl.applyToOptions = (n)=>{
        const a = hv(n, 100, Kl);
        return n.ease = a.ease, n.duration = Pe(a.duration), n.type = "keyframes", n;
    };
    function uf({ keyframes: n, velocity: a = 0, power: l = .8, timeConstant: r = 325, bounceDamping: u = 10, bounceStiffness: d = 500, modifyTarget: f, min: h, max: m, restDelta: p = .5, restSpeed: y }) {
        const v = n[0], x = {
            done: !1,
            value: v
        }, A = (Z)=>h !== void 0 && Z < h || m !== void 0 && Z > m, E = (Z)=>h === void 0 ? m : m === void 0 || Math.abs(h - Z) < Math.abs(m - Z) ? h : m;
        let M = l * a;
        const R = v + M, z = f === void 0 ? R : f(R);
        z !== R && (M = z - v);
        const B = (Z)=>-M * Math.exp(-Z / r), V = (Z)=>z + B(Z), P = (Z)=>{
            const Q = B(Z), it = V(Z);
            x.done = Math.abs(Q) <= p, x.value = x.done ? z : it;
        };
        let U, X;
        const H = (Z)=>{
            A(x.value) && (U = Z, X = Kl({
                keyframes: [
                    x.value,
                    E(x.value)
                ],
                velocity: mv(V, Z, x.value),
                damping: u,
                stiffness: d,
                restDelta: p,
                restSpeed: y
            }));
        };
        return H(0), {
            calculatedDuration: null,
            next: (Z)=>{
                let Q = !1;
                return !X && U === void 0 && (Q = !0, P(Z), H(Z)), U !== void 0 && Z >= U ? X.next(Z - U) : (!Q && P(Z), x);
            }
        };
    }
    function XE(n, a, l) {
        const r = [], u = l || Dn.mix || fv, d = n.length - 1;
        for(let f = 0; f < d; f++){
            let h = u(n[f], n[f + 1]);
            if (a) {
                const m = Array.isArray(a) ? a[f] || Ke : a;
                h = Il(m, h);
            }
            r.push(h);
        }
        return r;
    }
    function pv(n, a, { clamp: l = !0, ease: r, mixer: u } = {}) {
        const d = n.length;
        if (Yf(d === a.length), d === 1) return ()=>a[0];
        if (d === 2 && a[0] === a[1]) return ()=>a[1];
        const f = n[0] === n[1];
        n[0] > n[d - 1] && (n = [
            ...n
        ].reverse(), a = [
            ...a
        ].reverse());
        const h = XE(a, r, u), m = h.length, p = (y)=>{
            if (f && y < n[0]) return a[0];
            let v = 0;
            if (m > 1) for(; v < n.length - 2 && !(y < n[v + 1]); v++);
            const x = Mi(n[v], n[v + 1], y);
            return h[v](x);
        };
        return l ? (y)=>p(un(n[0], n[d - 1], y)) : p;
    }
    function gv(n, a) {
        const l = n[n.length - 1];
        for(let r = 1; r <= a; r++){
            const u = Mi(0, a, r);
            n.push(Ut(l, 1, u));
        }
    }
    function yv(n) {
        const a = [
            0
        ];
        return gv(a, n.length - 1), a;
    }
    function KE(n, a) {
        return n.map((l)=>l * a);
    }
    function PE(n, a) {
        return n.map(()=>a || W0).splice(0, n.length - 1);
    }
    function Vl({ duration: n = 300, keyframes: a, times: l, ease: r = "easeInOut" }) {
        const u = I0(r) ? r.map(qg) : qg(r), d = {
            done: !1,
            value: a[0]
        }, f = KE(l && l.length === a.length ? l : yv(a), n), h = pv(f, a, {
            ease: Array.isArray(u) ? u : PE(a, u)
        });
        return {
            calculatedDuration: n,
            next: (m)=>(d.value = h(m), d.done = m >= n, d)
        };
    }
    const ZE = (n)=>n !== null;
    function Wf(n, { repeat: a, repeatType: l = "loop" }, r, u = 1) {
        const d = n.filter(ZE), h = u < 0 || a && l !== "loop" && a % 2 === 1 ? 0 : d.length - 1;
        return !h || r === void 0 ? d[h] : r;
    }
    const QE = {
        decay: uf,
        inertia: uf,
        tween: Vl,
        keyframes: Vl,
        spring: Kl
    };
    function vv(n) {
        typeof n.type == "string" && (n.type = QE[n.type]);
    }
    class If {
        constructor(){
            this.updateFinished();
        }
        get finished() {
            return this._finished;
        }
        updateFinished() {
            this._finished = new Promise((a)=>{
                this.resolve = a;
            });
        }
        notifyFinished() {
            this.resolve();
        }
        then(a, l) {
            return this.finished.then(a, l);
        }
    }
    const FE = (n)=>n / 100;
    class td extends If {
        constructor(a){
            super(), this.state = "idle", this.startTime = null, this.isStopped = !1, this.currentTime = 0, this.holdTime = null, this.playbackSpeed = 1, this.stop = ()=>{
                const { motionValue: l } = this.options;
                l && l.updatedAt !== me.now() && this.tick(me.now()), this.isStopped = !0, this.state !== "idle" && (this.teardown(), this.options.onStop?.());
            }, this.options = a, this.initAnimation(), this.play(), a.autoplay === !1 && this.pause();
        }
        initAnimation() {
            const { options: a } = this;
            vv(a);
            const { type: l = Vl, repeat: r = 0, repeatDelay: u = 0, repeatType: d, velocity: f = 0 } = a;
            let { keyframes: h } = a;
            const m = l || Vl;
            m !== Vl && typeof h[0] != "number" && (this.mixKeyframes = Il(FE, fv(h[0], h[1])), h = [
                0,
                100
            ]);
            const p = m({
                ...a,
                keyframes: h
            });
            d === "mirror" && (this.mirroredGenerator = m({
                ...a,
                keyframes: [
                    ...h
                ].reverse(),
                velocity: -f
            })), p.calculatedDuration === null && (p.calculatedDuration = Jf(p));
            const { calculatedDuration: y } = p;
            this.calculatedDuration = y, this.resolvedDuration = y + u, this.totalDuration = this.resolvedDuration * (r + 1) - u, this.generator = p;
        }
        updateTime(a) {
            const l = Math.round(a - this.startTime) * this.playbackSpeed;
            this.holdTime !== null ? this.currentTime = this.holdTime : this.currentTime = l;
        }
        tick(a, l = !1) {
            const { generator: r, totalDuration: u, mixKeyframes: d, mirroredGenerator: f, resolvedDuration: h, calculatedDuration: m } = this;
            if (this.startTime === null) return r.next(0);
            const { delay: p = 0, keyframes: y, repeat: v, repeatType: x, repeatDelay: A, type: E, onUpdate: M, finalKeyframe: R } = this.options;
            this.speed > 0 ? this.startTime = Math.min(this.startTime, a) : this.speed < 0 && (this.startTime = Math.min(a - u / this.speed, this.startTime)), l ? this.currentTime = a : this.updateTime(a);
            const z = this.currentTime - p * (this.playbackSpeed >= 0 ? 1 : -1), B = this.playbackSpeed >= 0 ? z < 0 : z > u;
            this.currentTime = Math.max(z, 0), this.state === "finished" && this.holdTime === null && (this.currentTime = u);
            let V = this.currentTime, P = r;
            if (v) {
                const Z = Math.min(this.currentTime, u) / h;
                let Q = Math.floor(Z), it = Z % 1;
                !it && Z >= 1 && (it = 1), it === 1 && Q--, Q = Math.min(Q, v + 1), !!(Q % 2) && (x === "reverse" ? (it = 1 - it, A && (it -= A / h)) : x === "mirror" && (P = f)), V = un(0, 1, it) * h;
            }
            const U = B ? {
                done: !1,
                value: y[0]
            } : P.next(V);
            d && !B && (U.value = d(U.value));
            let { done: X } = U;
            !B && m !== null && (X = this.playbackSpeed >= 0 ? this.currentTime >= u : this.currentTime <= 0);
            const H = this.holdTime === null && (this.state === "finished" || this.state === "running" && X);
            return H && E !== uf && (U.value = Wf(y, this.options, R, this.speed)), M && M(U.value), H && this.finish(), U;
        }
        then(a, l) {
            return this.finished.then(a, l);
        }
        get duration() {
            return Xe(this.calculatedDuration);
        }
        get iterationDuration() {
            const { delay: a = 0 } = this.options || {};
            return this.duration + Xe(a);
        }
        get time() {
            return Xe(this.currentTime);
        }
        set time(a) {
            a = Pe(a), this.currentTime = a, this.startTime === null || this.holdTime !== null || this.playbackSpeed === 0 ? this.holdTime = a : this.driver && (this.startTime = this.driver.now() - a / this.playbackSpeed), this.driver ? this.driver.start(!1) : (this.startTime = 0, this.state = "paused", this.holdTime = a, this.tick(a));
        }
        get speed() {
            return this.playbackSpeed;
        }
        set speed(a) {
            const l = this.playbackSpeed !== a;
            l && this.driver && this.updateTime(me.now()), this.playbackSpeed = a, l && this.driver && (this.time = Xe(this.currentTime));
        }
        play() {
            if (this.isStopped) return;
            const { driver: a = VE, startTime: l } = this.options;
            this.driver || (this.driver = a((u)=>this.tick(u))), this.options.onPlay?.();
            const r = this.driver.now();
            this.state === "finished" ? (this.updateFinished(), this.startTime = r) : this.holdTime !== null ? this.startTime = r - this.holdTime : this.startTime || (this.startTime = l ?? r), this.state === "finished" && this.speed < 0 && (this.startTime += this.calculatedDuration), this.holdTime = null, this.state = "running", this.driver.start();
        }
        pause() {
            this.state = "paused", this.updateTime(me.now()), this.holdTime = this.currentTime;
        }
        complete() {
            this.state !== "running" && this.play(), this.state = "finished", this.holdTime = null;
        }
        finish() {
            this.notifyFinished(), this.teardown(), this.state = "finished", this.options.onComplete?.();
        }
        cancel() {
            this.holdTime = null, this.startTime = 0, this.tick(0), this.teardown(), this.options.onCancel?.();
        }
        teardown() {
            this.state = "idle", this.stopDriver(), this.startTime = this.holdTime = null;
        }
        stopDriver() {
            this.driver && (this.driver.stop(), this.driver = void 0);
        }
        sample(a) {
            return this.startTime = 0, this.tick(a, !0);
        }
        attachTimeline(a) {
            return this.options.allowFlatten && (this.options.type = "keyframes", this.options.ease = "linear", this.initAnimation()), this.driver?.stop(), a.observe(this);
        }
    }
    function $E(n) {
        for(let a = 1; a < n.length; a++)n[a] ?? (n[a] = n[a - 1]);
    }
    const za = (n)=>n * 180 / Math.PI, cf = (n)=>{
        const a = za(Math.atan2(n[1], n[0]));
        return ff(a);
    }, JE = {
        x: 4,
        y: 5,
        translateX: 4,
        translateY: 5,
        scaleX: 0,
        scaleY: 3,
        scale: (n)=>(Math.abs(n[0]) + Math.abs(n[3])) / 2,
        rotate: cf,
        rotateZ: cf,
        skewX: (n)=>za(Math.atan(n[1])),
        skewY: (n)=>za(Math.atan(n[2])),
        skew: (n)=>(Math.abs(n[1]) + Math.abs(n[2])) / 2
    }, ff = (n)=>(n = n % 360, n < 0 && (n += 360), n), Qg = cf, Fg = (n)=>Math.sqrt(n[0] * n[0] + n[1] * n[1]), $g = (n)=>Math.sqrt(n[4] * n[4] + n[5] * n[5]), WE = {
        x: 12,
        y: 13,
        z: 14,
        translateX: 12,
        translateY: 13,
        translateZ: 14,
        scaleX: Fg,
        scaleY: $g,
        scale: (n)=>(Fg(n) + $g(n)) / 2,
        rotateX: (n)=>ff(za(Math.atan2(n[6], n[5]))),
        rotateY: (n)=>ff(za(Math.atan2(-n[2], n[0]))),
        rotateZ: Qg,
        rotate: Qg,
        skewX: (n)=>za(Math.atan(n[4])),
        skewY: (n)=>za(Math.atan(n[1])),
        skew: (n)=>(Math.abs(n[1]) + Math.abs(n[4])) / 2
    };
    function df(n) {
        return n.includes("scale") ? 1 : 0;
    }
    function hf(n, a) {
        if (!n || n === "none") return df(a);
        const l = n.match(/^matrix3d\(([-\d.e\s,]+)\)$/u);
        let r, u;
        if (l) r = WE, u = l;
        else {
            const h = n.match(/^matrix\(([-\d.e\s,]+)\)$/u);
            r = JE, u = h;
        }
        if (!u) return df(a);
        const d = r[a], f = u[1].split(",").map(tA);
        return typeof d == "function" ? d(f) : f[d];
    }
    const IE = (n, a)=>{
        const { transform: l = "none" } = getComputedStyle(n);
        return hf(l, a);
    };
    function tA(n) {
        return parseFloat(n.trim());
    }
    const Ni = [
        "transformPerspective",
        "x",
        "y",
        "z",
        "translateX",
        "translateY",
        "translateZ",
        "scale",
        "scaleX",
        "scaleY",
        "rotate",
        "rotateX",
        "rotateY",
        "rotateZ",
        "skew",
        "skewX",
        "skewY"
    ], zi = new Set(Ni), Jg = (n)=>n === Oi || n === tt, eA = new Set([
        "x",
        "y",
        "z"
    ]), nA = Ni.filter((n)=>!eA.has(n));
    function aA(n) {
        const a = [];
        return nA.forEach((l)=>{
            const r = n.getValue(l);
            r !== void 0 && (a.push([
                l,
                r.get()
            ]), r.set(l.startsWith("scale") ? 1 : 0));
        }), a;
    }
    const ra = {
        width: ({ x: n }, { paddingLeft: a = "0", paddingRight: l = "0" })=>n.max - n.min - parseFloat(a) - parseFloat(l),
        height: ({ y: n }, { paddingTop: a = "0", paddingBottom: l = "0" })=>n.max - n.min - parseFloat(a) - parseFloat(l),
        top: (n, { top: a })=>parseFloat(a),
        left: (n, { left: a })=>parseFloat(a),
        bottom: ({ y: n }, { top: a })=>parseFloat(a) + (n.max - n.min),
        right: ({ x: n }, { left: a })=>parseFloat(a) + (n.max - n.min),
        x: (n, { transform: a })=>hf(a, "x"),
        y: (n, { transform: a })=>hf(a, "y")
    };
    ra.translateX = ra.x;
    ra.translateY = ra.y;
    const La = new Set;
    let mf = !1, pf = !1, gf = !1;
    function bv() {
        if (pf) {
            const n = Array.from(La).filter((r)=>r.needsMeasurement), a = new Set(n.map((r)=>r.element)), l = new Map;
            a.forEach((r)=>{
                const u = aA(r);
                u.length && (l.set(r, u), r.render());
            }), n.forEach((r)=>r.measureInitialState()), a.forEach((r)=>{
                r.render();
                const u = l.get(r);
                u && u.forEach(([d, f])=>{
                    r.getValue(d)?.set(f);
                });
            }), n.forEach((r)=>r.measureEndState()), n.forEach((r)=>{
                r.suspendedScrollY !== void 0 && window.scrollTo(0, r.suspendedScrollY);
            });
        }
        pf = !1, mf = !1, La.forEach((n)=>n.complete(gf)), La.clear();
    }
    function xv() {
        La.forEach((n)=>{
            n.readKeyframes(), n.needsMeasurement && (pf = !0);
        });
    }
    function iA() {
        gf = !0, xv(), bv(), gf = !1;
    }
    class ed {
        constructor(a, l, r, u, d, f = !1){
            this.state = "pending", this.isAsync = !1, this.needsMeasurement = !1, this.unresolvedKeyframes = [
                ...a
            ], this.onComplete = l, this.name = r, this.motionValue = u, this.element = d, this.isAsync = f;
        }
        scheduleResolve() {
            this.state = "scheduled", this.isAsync ? (La.add(this), mf || (mf = !0, Ot.read(xv), Ot.resolveKeyframes(bv))) : (this.readKeyframes(), this.complete());
        }
        readKeyframes() {
            const { unresolvedKeyframes: a, name: l, element: r, motionValue: u } = this;
            if (a[0] === null) {
                const d = u?.get(), f = a[a.length - 1];
                if (d !== void 0) a[0] = d;
                else if (r && l) {
                    const h = r.readValue(l, f);
                    h != null && (a[0] = h);
                }
                a[0] === void 0 && (a[0] = f), u && d === void 0 && u.set(a[0]);
            }
            $E(a);
        }
        setFinalKeyframe() {}
        measureInitialState() {}
        renderEndStyles() {}
        measureEndState() {}
        complete(a = !1) {
            this.state = "complete", this.onComplete(this.unresolvedKeyframes, this.finalKeyframe, a), La.delete(this);
        }
        cancel() {
            this.state === "scheduled" && (La.delete(this), this.state = "pending");
        }
        resume() {
            this.state === "pending" && this.scheduleResolve();
        }
    }
    const lA = (n)=>n.startsWith("--");
    function Sv(n, a, l) {
        lA(a) ? n.style.setProperty(a, l) : n.style[a] = l;
    }
    const sA = {};
    function Tv(n, a) {
        const l = k0(n);
        return ()=>sA[a] ?? l();
    }
    const rA = Tv(()=>window.ScrollTimeline !== void 0, "scrollTimeline"), Ev = Tv(()=>{
        try {
            document.createElement("div").animate({
                opacity: 0
            }, {
                easing: "linear(0, 1)"
            });
        } catch  {
            return !1;
        }
        return !0;
    }, "linearEasing"), Ol = ([n, a, l, r])=>`cubic-bezier(${n}, ${a}, ${l}, ${r})`, Wg = {
        linear: "linear",
        ease: "ease",
        easeIn: "ease-in",
        easeOut: "ease-out",
        easeInOut: "ease-in-out",
        circIn: Ol([
            0,
            .65,
            .55,
            1
        ]),
        circOut: Ol([
            .55,
            0,
            1,
            .45
        ]),
        backIn: Ol([
            .31,
            .01,
            .66,
            -.59
        ]),
        backOut: Ol([
            .33,
            1.53,
            .69,
            .99
        ])
    };
    function Av(n, a) {
        if (n) return typeof n == "function" ? Ev() ? dv(n, a) : "ease-out" : ev(n) ? Ol(n) : Array.isArray(n) ? n.map((l)=>Av(l, a) || Wg.easeOut) : Wg[n];
    }
    function oA(n, a, l, { delay: r = 0, duration: u = 300, repeat: d = 0, repeatType: f = "loop", ease: h = "easeOut", times: m } = {}, p = void 0) {
        const y = {
            [a]: l
        };
        m && (y.offset = m);
        const v = Av(h, u);
        Array.isArray(v) && (y.easing = v);
        const x = {
            delay: r,
            duration: u,
            easing: Array.isArray(v) ? "linear" : v,
            fill: "both",
            iterations: d + 1,
            direction: f === "reverse" ? "alternate" : "normal"
        };
        return p && (x.pseudoElement = p), n.animate(y, x);
    }
    function nd(n) {
        return typeof n == "function" && "applyToOptions" in n;
    }
    function uA({ type: n, ...a }) {
        return nd(n) && Ev() ? n.applyToOptions(a) : (a.duration ?? (a.duration = 300), a.ease ?? (a.ease = "easeOut"), a);
    }
    class Cv extends If {
        constructor(a){
            if (super(), this.finishedTime = null, this.isStopped = !1, this.manualStartTime = null, !a) return;
            const { element: l, name: r, keyframes: u, pseudoElement: d, allowFlatten: f = !1, finalKeyframe: h, onComplete: m } = a;
            this.isPseudoElement = !!d, this.allowFlatten = f, this.options = a, Yf(typeof a.type != "string");
            const p = uA(a);
            this.animation = oA(l, r, u, p, d), p.autoplay === !1 && this.animation.pause(), this.animation.onfinish = ()=>{
                if (this.finishedTime = this.time, !d) {
                    const y = Wf(u, this.options, h, this.speed);
                    this.updateMotionValue && this.updateMotionValue(y), Sv(l, r, y), this.animation.cancel();
                }
                m?.(), this.notifyFinished();
            };
        }
        play() {
            this.isStopped || (this.manualStartTime = null, this.animation.play(), this.state === "finished" && this.updateFinished());
        }
        pause() {
            this.animation.pause();
        }
        complete() {
            this.animation.finish?.();
        }
        cancel() {
            try {
                this.animation.cancel();
            } catch  {}
        }
        stop() {
            if (this.isStopped) return;
            this.isStopped = !0;
            const { state: a } = this;
            a === "idle" || a === "finished" || (this.updateMotionValue ? this.updateMotionValue() : this.commitStyles(), this.isPseudoElement || this.cancel());
        }
        commitStyles() {
            const a = this.options?.element;
            !this.isPseudoElement && a?.isConnected && this.animation.commitStyles?.();
        }
        get duration() {
            const a = this.animation.effect?.getComputedTiming?.().duration || 0;
            return Xe(Number(a));
        }
        get iterationDuration() {
            const { delay: a = 0 } = this.options || {};
            return this.duration + Xe(a);
        }
        get time() {
            return Xe(Number(this.animation.currentTime) || 0);
        }
        set time(a) {
            const l = this.finishedTime !== null;
            this.manualStartTime = null, this.finishedTime = null, this.animation.currentTime = Pe(a), l && this.animation.pause();
        }
        get speed() {
            return this.animation.playbackRate;
        }
        set speed(a) {
            a < 0 && (this.finishedTime = null), this.animation.playbackRate = a;
        }
        get state() {
            return this.finishedTime !== null ? "finished" : this.animation.playState;
        }
        get startTime() {
            return this.manualStartTime ?? Number(this.animation.startTime);
        }
        set startTime(a) {
            this.manualStartTime = this.animation.startTime = a;
        }
        attachTimeline({ timeline: a, rangeStart: l, rangeEnd: r, observe: u }) {
            return this.allowFlatten && this.animation.effect?.updateTiming({
                easing: "linear"
            }), this.animation.onfinish = null, a && rA() ? (this.animation.timeline = a, l && (this.animation.rangeStart = l), r && (this.animation.rangeEnd = r), Ke) : u(this);
        }
    }
    const wv = {
        anticipate: F0,
        backInOut: Q0,
        circInOut: J0
    };
    function cA(n) {
        return n in wv;
    }
    function fA(n) {
        typeof n.ease == "string" && cA(n.ease) && (n.ease = wv[n.ease]);
    }
    const Gc = 10;
    class dA extends Cv {
        constructor(a){
            fA(a), vv(a), super(a), a.startTime !== void 0 && (this.startTime = a.startTime), this.options = a;
        }
        updateMotionValue(a) {
            const { motionValue: l, onUpdate: r, onComplete: u, element: d, ...f } = this.options;
            if (!l) return;
            if (a !== void 0) {
                l.set(a);
                return;
            }
            const h = new td({
                ...f,
                autoplay: !1
            }), m = Math.max(Gc, me.now() - this.startTime), p = un(0, Gc, m - Gc), y = h.sample(m).value, { name: v } = this.options;
            d && v && Sv(d, v, y), l.setWithVelocity(h.sample(Math.max(0, m - p)).value, y, p), h.stop();
        }
    }
    const Ig = (n, a)=>a === "zIndex" ? !1 : !!(typeof n == "number" || Array.isArray(n) || typeof n == "string" && (Ie.test(n) || n === "0") && !n.startsWith("url("));
    function hA(n) {
        const a = n[0];
        if (n.length === 1) return !0;
        for(let l = 0; l < n.length; l++)if (n[l] !== a) return !0;
    }
    function mA(n, a, l, r) {
        const u = n[0];
        if (u === null) return !1;
        if (a === "display" || a === "visibility") return !0;
        const d = n[n.length - 1], f = Ig(u, a), h = Ig(d, a);
        return !f || !h ? !1 : hA(n) || (l === "spring" || nd(l)) && r;
    }
    function yf(n) {
        n.duration = 0, n.type = "keyframes";
    }
    const pA = new Set([
        "opacity",
        "clipPath",
        "filter",
        "transform"
    ]), gA = k0(()=>Object.hasOwnProperty.call(Element.prototype, "animate"));
    function yA(n) {
        const { motionValue: a, name: l, repeatDelay: r, repeatType: u, damping: d, type: f } = n;
        if (!(a?.owner?.current instanceof HTMLElement)) return !1;
        const { onUpdate: m, transformTemplate: p } = a.owner.getProps();
        return gA() && l && pA.has(l) && (l !== "transform" || !p) && !m && !r && u !== "mirror" && d !== 0 && f !== "inertia";
    }
    const vA = 40;
    class bA extends If {
        constructor({ autoplay: a = !0, delay: l = 0, type: r = "keyframes", repeat: u = 0, repeatDelay: d = 0, repeatType: f = "loop", keyframes: h, name: m, motionValue: p, element: y, ...v }){
            super(), this.stop = ()=>{
                this._animation && (this._animation.stop(), this.stopTimeline?.()), this.keyframeResolver?.cancel();
            }, this.createdAt = me.now();
            const x = {
                autoplay: a,
                delay: l,
                type: r,
                repeat: u,
                repeatDelay: d,
                repeatType: f,
                name: m,
                motionValue: p,
                element: y,
                ...v
            }, A = y?.KeyframeResolver || ed;
            this.keyframeResolver = new A(h, (E, M, R)=>this.onKeyframesResolved(E, M, x, !R), m, p, y), this.keyframeResolver?.scheduleResolve();
        }
        onKeyframesResolved(a, l, r, u) {
            this.keyframeResolver = void 0;
            const { name: d, type: f, velocity: h, delay: m, isHandoff: p, onUpdate: y } = r;
            this.resolvedAt = me.now(), mA(a, d, f, h) || ((Dn.instantAnimations || !m) && y?.(Wf(a, r, l)), a[0] = a[a.length - 1], yf(r), r.repeat = 0);
            const x = {
                startTime: u ? this.resolvedAt ? this.resolvedAt - this.createdAt > vA ? this.resolvedAt : this.createdAt : this.createdAt : void 0,
                finalKeyframe: l,
                ...r,
                keyframes: a
            }, A = !p && yA(x), E = x.motionValue?.owner?.current, M = A ? new dA({
                ...x,
                element: E
            }) : new td(x);
            M.finished.then(()=>{
                this.notifyFinished();
            }).catch(Ke), this.pendingTimeline && (this.stopTimeline = M.attachTimeline(this.pendingTimeline), this.pendingTimeline = void 0), this._animation = M;
        }
        get finished() {
            return this._animation ? this.animation.finished : this._finished;
        }
        then(a, l) {
            return this.finished.finally(a).then(()=>{});
        }
        get animation() {
            return this._animation || (this.keyframeResolver?.resume(), iA()), this._animation;
        }
        get duration() {
            return this.animation.duration;
        }
        get iterationDuration() {
            return this.animation.iterationDuration;
        }
        get time() {
            return this.animation.time;
        }
        set time(a) {
            this.animation.time = a;
        }
        get speed() {
            return this.animation.speed;
        }
        get state() {
            return this.animation.state;
        }
        set speed(a) {
            this.animation.speed = a;
        }
        get startTime() {
            return this.animation.startTime;
        }
        attachTimeline(a) {
            return this._animation ? this.stopTimeline = this.animation.attachTimeline(a) : this.pendingTimeline = a, ()=>this.stop();
        }
        play() {
            this.animation.play();
        }
        pause() {
            this.animation.pause();
        }
        complete() {
            this.animation.complete();
        }
        cancel() {
            this._animation && this.animation.cancel(), this.keyframeResolver?.cancel();
        }
    }
    class xA {
        constructor(a){
            this.stop = ()=>this.runAll("stop"), this.animations = a.filter(Boolean);
        }
        get finished() {
            return Promise.all(this.animations.map((a)=>a.finished));
        }
        getAll(a) {
            return this.animations[0][a];
        }
        setAll(a, l) {
            for(let r = 0; r < this.animations.length; r++)this.animations[r][a] = l;
        }
        attachTimeline(a) {
            const l = this.animations.map((r)=>r.attachTimeline(a));
            return ()=>{
                l.forEach((r, u)=>{
                    r && r(), this.animations[u].stop();
                });
            };
        }
        get time() {
            return this.getAll("time");
        }
        set time(a) {
            this.setAll("time", a);
        }
        get speed() {
            return this.getAll("speed");
        }
        set speed(a) {
            this.setAll("speed", a);
        }
        get state() {
            return this.getAll("state");
        }
        get startTime() {
            return this.getAll("startTime");
        }
        get duration() {
            return ty(this.animations, "duration");
        }
        get iterationDuration() {
            return ty(this.animations, "iterationDuration");
        }
        runAll(a) {
            this.animations.forEach((l)=>l[a]());
        }
        play() {
            this.runAll("play");
        }
        pause() {
            this.runAll("pause");
        }
        cancel() {
            this.runAll("cancel");
        }
        complete() {
            this.runAll("complete");
        }
    }
    function ty(n, a) {
        let l = 0;
        for(let r = 0; r < n.length; r++){
            const u = n[r][a];
            u !== null && u > l && (l = u);
        }
        return l;
    }
    class SA extends xA {
        then(a, l) {
            return this.finished.finally(a).then(()=>{});
        }
    }
    function _v(n, a, l, r = 0, u = 1) {
        const d = Array.from(n).sort((p, y)=>p.sortNodePosition(y)).indexOf(a), f = n.size, h = (f - 1) * r;
        return typeof l == "function" ? l(d, f) : u === 1 ? d * r : h - d * r;
    }
    const TA = /^var\(--(?:([\w-]+)|([\w-]+), ?([a-zA-Z\d ()%#.,-]+))\)/u;
    function EA(n) {
        const a = TA.exec(n);
        if (!a) return [
            , 
        ];
        const [, l, r, u] = a;
        return [
            `--${l ?? r}`,
            u
        ];
    }
    function Rv(n, a, l = 1) {
        const [r, u] = EA(n);
        if (!r) return;
        const d = window.getComputedStyle(a).getPropertyValue(r);
        if (d) {
            const f = d.trim();
            return H0(f) ? parseFloat(f) : f;
        }
        return Zf(u) ? Rv(u, a, l + 1) : u;
    }
    const AA = {
        type: "spring",
        stiffness: 500,
        damping: 25,
        restSpeed: 10
    }, CA = (n)=>({
            type: "spring",
            stiffness: 550,
            damping: n === 0 ? 2 * Math.sqrt(550) : 30,
            restSpeed: 10
        }), wA = {
        type: "keyframes",
        duration: .8
    }, _A = {
        type: "keyframes",
        ease: [
            .25,
            .1,
            .35,
            1
        ],
        duration: .3
    }, RA = (n, { keyframes: a })=>a.length > 2 ? wA : zi.has(n) ? n.startsWith("scale") ? CA(a[1]) : AA : _A, MA = (n)=>n !== null;
    function DA(n, { repeat: a, repeatType: l = "loop" }, r) {
        const u = n.filter(MA), d = a && l !== "loop" && a % 2 === 1 ? 0 : u.length - 1;
        return u[d];
    }
    function Mv(n, a) {
        if (n?.inherit && a) {
            const { inherit: l, ...r } = n;
            return {
                ...a,
                ...r
            };
        }
        return n;
    }
    function ad(n, a) {
        const l = n?.[a] ?? n?.default ?? n;
        return l !== n ? Mv(l, n) : l;
    }
    function jA({ when: n, delay: a, delayChildren: l, staggerChildren: r, staggerDirection: u, repeat: d, repeatType: f, repeatDelay: h, from: m, elapsed: p, ...y }) {
        return !!Object.keys(y).length;
    }
    const id = (n, a, l, r = {}, u, d)=>(f)=>{
            const h = ad(r, n) || {}, m = h.delay || r.delay || 0;
            let { elapsed: p = 0 } = r;
            p = p - Pe(m);
            const y = {
                keyframes: Array.isArray(l) ? l : [
                    null,
                    l
                ],
                ease: "easeOut",
                velocity: a.getVelocity(),
                ...h,
                delay: -p,
                onUpdate: (x)=>{
                    a.set(x), h.onUpdate && h.onUpdate(x);
                },
                onComplete: ()=>{
                    f(), h.onComplete && h.onComplete();
                },
                name: n,
                motionValue: a,
                element: d ? void 0 : u
            };
            jA(h) || Object.assign(y, RA(n, y)), y.duration && (y.duration = Pe(y.duration)), y.repeatDelay && (y.repeatDelay = Pe(y.repeatDelay)), y.from !== void 0 && (y.keyframes[0] = y.from);
            let v = !1;
            if ((y.type === !1 || y.duration === 0 && !y.repeatDelay) && (yf(y), y.delay === 0 && (v = !0)), (Dn.instantAnimations || Dn.skipAnimations || u?.shouldSkipAnimations) && (v = !0, yf(y), y.delay = 0), y.allowFlatten = !h.type && !h.ease, v && !d && a.get() !== void 0) {
                const x = DA(y.keyframes, h);
                if (x !== void 0) {
                    Ot.update(()=>{
                        y.onUpdate(x), y.onComplete();
                    });
                    return;
                }
            }
            return h.isSync ? new td(y) : new bA(y);
        };
    function ey(n) {
        const a = [
            {},
            {}
        ];
        return n?.values.forEach((l, r)=>{
            a[0][r] = l.get(), a[1][r] = l.getVelocity();
        }), a;
    }
    function ld(n, a, l, r) {
        if (typeof a == "function") {
            const [u, d] = ey(r);
            a = a(l !== void 0 ? l : n.custom, u, d);
        }
        if (typeof a == "string" && (a = n.variants && n.variants[a]), typeof a == "function") {
            const [u, d] = ey(r);
            a = a(l !== void 0 ? l : n.custom, u, d);
        }
        return a;
    }
    function _i(n, a, l) {
        const r = n.getProps();
        return ld(r, a, l !== void 0 ? l : r.custom, n);
    }
    const Dv = new Set([
        "width",
        "height",
        "top",
        "left",
        "right",
        "bottom",
        ...Ni
    ]), ny = 30, OA = (n)=>!isNaN(parseFloat(n)), Bl = {
        current: void 0
    };
    class NA {
        constructor(a, l = {}){
            this.canTrackVelocity = null, this.events = {}, this.updateAndNotify = (r)=>{
                const u = me.now();
                if (this.updatedAt !== u && this.setPrevFrameValue(), this.prev = this.current, this.setCurrent(r), this.current !== this.prev && (this.events.change?.notify(this.current), this.dependents)) for (const d of this.dependents)d.dirty();
            }, this.hasAnimated = !1, this.setCurrent(a), this.owner = l.owner;
        }
        setCurrent(a) {
            this.current = a, this.updatedAt = me.now(), this.canTrackVelocity === null && a !== void 0 && (this.canTrackVelocity = OA(this.current));
        }
        setPrevFrameValue(a = this.current) {
            this.prevFrameValue = a, this.prevUpdatedAt = this.updatedAt;
        }
        onChange(a) {
            return this.on("change", a);
        }
        on(a, l) {
            this.events[a] || (this.events[a] = new Xf);
            const r = this.events[a].add(l);
            return a === "change" ? ()=>{
                r(), Ot.read(()=>{
                    this.events.change.getSize() || this.stop();
                });
            } : r;
        }
        clearListeners() {
            for(const a in this.events)this.events[a].clear();
        }
        attach(a, l) {
            this.passiveEffect = a, this.stopPassiveEffect = l;
        }
        set(a) {
            this.passiveEffect ? this.passiveEffect(a, this.updateAndNotify) : this.updateAndNotify(a);
        }
        setWithVelocity(a, l, r) {
            this.set(l), this.prev = void 0, this.prevFrameValue = a, this.prevUpdatedAt = this.updatedAt - r;
        }
        jump(a, l = !0) {
            this.updateAndNotify(a), this.prev = a, this.prevUpdatedAt = this.prevFrameValue = void 0, l && this.stop(), this.stopPassiveEffect && this.stopPassiveEffect();
        }
        dirty() {
            this.events.change?.notify(this.current);
        }
        addDependent(a) {
            this.dependents || (this.dependents = new Set), this.dependents.add(a);
        }
        removeDependent(a) {
            this.dependents && this.dependents.delete(a);
        }
        get() {
            return Bl.current && Bl.current.push(this), this.current;
        }
        getPrevious() {
            return this.prev;
        }
        getVelocity() {
            const a = me.now();
            if (!this.canTrackVelocity || this.prevFrameValue === void 0 || a - this.updatedAt > ny) return 0;
            const l = Math.min(this.updatedAt - this.prevUpdatedAt, ny);
            return Y0(parseFloat(this.current) - parseFloat(this.prevFrameValue), l);
        }
        start(a) {
            return this.stop(), new Promise((l)=>{
                this.hasAnimated = !0, this.animation = a(l), this.events.animationStart && this.events.animationStart.notify();
            }).then(()=>{
                this.events.animationComplete && this.events.animationComplete.notify(), this.clearAnimation();
            });
        }
        stop() {
            this.animation && (this.animation.stop(), this.events.animationCancel && this.events.animationCancel.notify()), this.clearAnimation();
        }
        isAnimating() {
            return !!this.animation;
        }
        clearAnimation() {
            delete this.animation;
        }
        destroy() {
            this.dependents?.clear(), this.events.destroy?.notify(), this.clearListeners(), this.stop(), this.stopPassiveEffect && this.stopPassiveEffect();
        }
    }
    function oa(n, a) {
        return new NA(n, a);
    }
    const vf = (n)=>Array.isArray(n);
    function zA(n, a, l) {
        n.hasValue(a) ? n.getValue(a).set(l) : n.addValue(a, oa(l));
    }
    function LA(n) {
        return vf(n) ? n[n.length - 1] || 0 : n;
    }
    function VA(n, a) {
        const l = _i(n, a);
        let { transitionEnd: r = {}, transition: u = {}, ...d } = l || {};
        d = {
            ...d,
            ...r
        };
        for(const f in d){
            const h = LA(d[f]);
            zA(n, f, h);
        }
    }
    const ie = (n)=>!!(n && n.getVelocity);
    function BA(n) {
        return !!(ie(n) && n.add);
    }
    function bf(n, a) {
        const l = n.getValue("willChange");
        if (BA(l)) return l.add(a);
        if (!l && Dn.WillChange) {
            const r = new Dn.WillChange("auto");
            n.addValue("willChange", r), r.add(a);
        }
    }
    function sd(n) {
        return n.replace(/([A-Z])/g, (a)=>`-${a.toLowerCase()}`);
    }
    const UA = "framerAppearId", jv = "data-" + sd(UA);
    function Ov(n) {
        return n.props[jv];
    }
    function HA({ protectedKeys: n, needsAnimating: a }, l) {
        const r = n.hasOwnProperty(l) && a[l] !== !0;
        return a[l] = !1, r;
    }
    function rd(n, a, { delay: l = 0, transitionOverride: r, type: u } = {}) {
        let { transition: d, transitionEnd: f, ...h } = a;
        const m = n.getDefaultTransition();
        d = d ? Mv(d, m) : m;
        const p = d?.reduceMotion;
        r && (d = r);
        const y = [], v = u && n.animationState && n.animationState.getState()[u];
        for(const x in h){
            const A = n.getValue(x, n.latestValues[x] ?? null), E = h[x];
            if (E === void 0 || v && HA(v, x)) continue;
            const M = {
                delay: l,
                ...ad(d || {}, x)
            }, R = A.get();
            if (R !== void 0 && !A.isAnimating && !Array.isArray(E) && E === R && !M.velocity) continue;
            let z = !1;
            if (window.MotionHandoffAnimation) {
                const P = Ov(n);
                if (P) {
                    const U = window.MotionHandoffAnimation(P, x, Ot);
                    U !== null && (M.startTime = U, z = !0);
                }
            }
            bf(n, x);
            const B = p ?? n.shouldReduceMotion;
            A.start(id(x, A, E, B && Dv.has(x) ? {
                type: !1
            } : M, n, z));
            const V = A.animation;
            V && y.push(V);
        }
        if (f) {
            const x = ()=>Ot.update(()=>{
                    f && VA(n, f);
                });
            y.length ? Promise.all(y).then(x) : x();
        }
        return y;
    }
    function xf(n, a, l = {}) {
        const r = _i(n, a, l.type === "exit" ? n.presenceContext?.custom : void 0);
        let { transition: u = n.getDefaultTransition() || {} } = r || {};
        l.transitionOverride && (u = l.transitionOverride);
        const d = r ? ()=>Promise.all(rd(n, r, l)) : ()=>Promise.resolve(), f = n.variantChildren && n.variantChildren.size ? (m = 0)=>{
            const { delayChildren: p = 0, staggerChildren: y, staggerDirection: v } = u;
            return GA(n, a, m, p, y, v, l);
        } : ()=>Promise.resolve(), { when: h } = u;
        if (h) {
            const [m, p] = h === "beforeChildren" ? [
                d,
                f
            ] : [
                f,
                d
            ];
            return m().then(()=>p());
        } else return Promise.all([
            d(),
            f(l.delay)
        ]);
    }
    function GA(n, a, l = 0, r = 0, u = 0, d = 1, f) {
        const h = [];
        for (const m of n.variantChildren)m.notify("AnimationStart", a), h.push(xf(m, a, {
            ...f,
            delay: l + (typeof r == "function" ? 0 : r) + _v(n.variantChildren, m, r, u, d)
        }).then(()=>m.notify("AnimationComplete", a)));
        return Promise.all(h);
    }
    function qA(n, a, l = {}) {
        n.notify("AnimationStart", a);
        let r;
        if (Array.isArray(a)) {
            const u = a.map((d)=>xf(n, d, l));
            r = Promise.all(u);
        } else if (typeof a == "string") r = xf(n, a, l);
        else {
            const u = typeof a == "function" ? _i(n, a, l.custom) : a;
            r = Promise.all(rd(n, u, l));
        }
        return r.then(()=>{
            n.notify("AnimationComplete", a);
        });
    }
    const kA = {
        test: (n)=>n === "auto",
        parse: (n)=>n
    }, Nv = (n)=>(a)=>a.test(n), zv = [
        Oi,
        tt,
        rn,
        sa,
        xE,
        bE,
        kA
    ], ay = (n)=>zv.find(Nv(n));
    function YA(n) {
        return typeof n == "number" ? n === 0 : n !== null ? n === "none" || n === "0" || q0(n) : !0;
    }
    const XA = new Set([
        "brightness",
        "contrast",
        "saturate",
        "opacity"
    ]);
    function KA(n) {
        const [a, l] = n.slice(0, -1).split("(");
        if (a === "drop-shadow") return n;
        const [r] = l.match(Qf) || [];
        if (!r) return n;
        const u = l.replace(r, "");
        let d = XA.has(a) ? 1 : 0;
        return r !== l && (d *= 100), a + "(" + d + u + ")";
    }
    const PA = /\b([a-z-]*)\(.*?\)/gu, Sf = {
        ...Ie,
        getAnimatableNone: (n)=>{
            const a = n.match(PA);
            return a ? a.map(KA).join(" ") : n;
        }
    }, Tf = {
        ...Ie,
        getAnimatableNone: (n)=>{
            const a = Ie.parse(n);
            return Ie.createTransformer(n)(a.map((r)=>typeof r == "number" ? 0 : typeof r == "object" ? {
                    ...r,
                    alpha: 1
                } : r));
        }
    }, iy = {
        ...Oi,
        transform: Math.round
    }, ZA = {
        rotate: sa,
        rotateX: sa,
        rotateY: sa,
        rotateZ: sa,
        scale: Ar,
        scaleX: Ar,
        scaleY: Ar,
        scaleZ: Ar,
        skew: sa,
        skewX: sa,
        skewY: sa,
        distance: tt,
        translateX: tt,
        translateY: tt,
        translateZ: tt,
        x: tt,
        y: tt,
        z: tt,
        perspective: tt,
        transformPerspective: tt,
        opacity: Yl,
        originX: Yg,
        originY: Yg,
        originZ: tt
    }, od = {
        borderWidth: tt,
        borderTopWidth: tt,
        borderRightWidth: tt,
        borderBottomWidth: tt,
        borderLeftWidth: tt,
        borderRadius: tt,
        borderTopLeftRadius: tt,
        borderTopRightRadius: tt,
        borderBottomRightRadius: tt,
        borderBottomLeftRadius: tt,
        width: tt,
        maxWidth: tt,
        height: tt,
        maxHeight: tt,
        top: tt,
        right: tt,
        bottom: tt,
        left: tt,
        inset: tt,
        insetBlock: tt,
        insetBlockStart: tt,
        insetBlockEnd: tt,
        insetInline: tt,
        insetInlineStart: tt,
        insetInlineEnd: tt,
        padding: tt,
        paddingTop: tt,
        paddingRight: tt,
        paddingBottom: tt,
        paddingLeft: tt,
        paddingBlock: tt,
        paddingBlockStart: tt,
        paddingBlockEnd: tt,
        paddingInline: tt,
        paddingInlineStart: tt,
        paddingInlineEnd: tt,
        margin: tt,
        marginTop: tt,
        marginRight: tt,
        marginBottom: tt,
        marginLeft: tt,
        marginBlock: tt,
        marginBlockStart: tt,
        marginBlockEnd: tt,
        marginInline: tt,
        marginInlineStart: tt,
        marginInlineEnd: tt,
        fontSize: tt,
        backgroundPositionX: tt,
        backgroundPositionY: tt,
        ...ZA,
        zIndex: iy,
        fillOpacity: Yl,
        strokeOpacity: Yl,
        numOctaves: iy
    }, QA = {
        ...od,
        color: Wt,
        backgroundColor: Wt,
        outlineColor: Wt,
        fill: Wt,
        stroke: Wt,
        borderColor: Wt,
        borderTopColor: Wt,
        borderRightColor: Wt,
        borderBottomColor: Wt,
        borderLeftColor: Wt,
        filter: Sf,
        WebkitFilter: Sf,
        mask: Tf,
        WebkitMask: Tf
    }, Lv = (n)=>QA[n], FA = new Set([
        Sf,
        Tf
    ]);
    function Vv(n, a) {
        let l = Lv(n);
        return FA.has(l) || (l = Ie), l.getAnimatableNone ? l.getAnimatableNone(a) : void 0;
    }
    const $A = new Set([
        "auto",
        "none",
        "0"
    ]);
    function JA(n, a, l) {
        let r = 0, u;
        for(; r < n.length && !u;){
            const d = n[r];
            typeof d == "string" && !$A.has(d) && Xl(d).values.length && (u = n[r]), r++;
        }
        if (u && l) for (const d of a)n[d] = Vv(l, u);
    }
    class WA extends ed {
        constructor(a, l, r, u, d){
            super(a, l, r, u, d, !0);
        }
        readKeyframes() {
            const { unresolvedKeyframes: a, element: l, name: r } = this;
            if (!l || !l.current) return;
            super.readKeyframes();
            for(let y = 0; y < a.length; y++){
                let v = a[y];
                if (typeof v == "string" && (v = v.trim(), Zf(v))) {
                    const x = Rv(v, l.current);
                    x !== void 0 && (a[y] = x), y === a.length - 1 && (this.finalKeyframe = v);
                }
            }
            if (this.resolveNoneKeyframes(), !Dv.has(r) || a.length !== 2) return;
            const [u, d] = a, f = ay(u), h = ay(d), m = kg(u), p = kg(d);
            if (m !== p && ra[r]) {
                this.needsMeasurement = !0;
                return;
            }
            if (f !== h) if (Jg(f) && Jg(h)) for(let y = 0; y < a.length; y++){
                const v = a[y];
                typeof v == "string" && (a[y] = parseFloat(v));
            }
            else ra[r] && (this.needsMeasurement = !0);
        }
        resolveNoneKeyframes() {
            const { unresolvedKeyframes: a, name: l } = this, r = [];
            for(let u = 0; u < a.length; u++)(a[u] === null || YA(a[u])) && r.push(u);
            r.length && JA(a, r, l);
        }
        measureInitialState() {
            const { element: a, unresolvedKeyframes: l, name: r } = this;
            if (!a || !a.current) return;
            r === "height" && (this.suspendedScrollY = window.pageYOffset), this.measuredOrigin = ra[r](a.measureViewportBox(), window.getComputedStyle(a.current)), l[0] = this.measuredOrigin;
            const u = l[l.length - 1];
            u !== void 0 && a.getValue(r, u).jump(u, !1);
        }
        measureEndState() {
            const { element: a, name: l, unresolvedKeyframes: r } = this;
            if (!a || !a.current) return;
            const u = a.getValue(l);
            u && u.jump(this.measuredOrigin, !1);
            const d = r.length - 1, f = r[d];
            r[d] = ra[l](a.measureViewportBox(), window.getComputedStyle(a.current)), f !== null && this.finalKeyframe === void 0 && (this.finalKeyframe = f), this.removedTransforms?.length && this.removedTransforms.forEach(([h, m])=>{
                a.getValue(h).set(m);
            }), this.resolveNoneKeyframes();
        }
    }
    const IA = new Set([
        "opacity",
        "clipPath",
        "filter",
        "transform"
    ]);
    function ud(n, a, l) {
        if (n == null) return [];
        if (n instanceof EventTarget) return [
            n
        ];
        if (typeof n == "string") {
            let r = document;
            a && (r = a.current);
            const u = l?.[n] ?? r.querySelectorAll(n);
            return u ? Array.from(u) : [];
        }
        return Array.from(n).filter((r)=>r != null);
    }
    const Bv = (n, a)=>a && typeof n == "number" ? a.transform(n) : n;
    function Ef(n) {
        return G0(n) && "offsetHeight" in n;
    }
    const { schedule: cd } = nv(queueMicrotask, !1), We = {
        x: !1,
        y: !1
    };
    function Uv() {
        return We.x || We.y;
    }
    function t2(n) {
        return n === "x" || n === "y" ? We[n] ? null : (We[n] = !0, ()=>{
            We[n] = !1;
        }) : We.x || We.y ? null : (We.x = We.y = !0, ()=>{
            We.x = We.y = !1;
        });
    }
    function Hv(n, a) {
        const l = ud(n), r = new AbortController, u = {
            passive: !0,
            ...a,
            signal: r.signal
        };
        return [
            l,
            u,
            ()=>r.abort()
        ];
    }
    function e2(n) {
        return !(n.pointerType === "touch" || Uv());
    }
    function n2(n, a, l = {}) {
        const [r, u, d] = Hv(n, l);
        return r.forEach((f)=>{
            let h = !1, m = !1, p;
            const y = ()=>{
                f.removeEventListener("pointerleave", E);
            }, v = (R)=>{
                p && (p(R), p = void 0), y();
            }, x = (R)=>{
                h = !1, window.removeEventListener("pointerup", x), window.removeEventListener("pointercancel", x), m && (m = !1, v(R));
            }, A = ()=>{
                h = !0, window.addEventListener("pointerup", x, u), window.addEventListener("pointercancel", x, u);
            }, E = (R)=>{
                if (R.pointerType !== "touch") {
                    if (h) {
                        m = !0;
                        return;
                    }
                    v(R);
                }
            }, M = (R)=>{
                if (!e2(R)) return;
                m = !1;
                const z = a(f, R);
                typeof z == "function" && (p = z, f.addEventListener("pointerleave", E, u));
            };
            f.addEventListener("pointerenter", M, u), f.addEventListener("pointerdown", A, u);
        }), d;
    }
    const Gv = (n, a)=>a ? n === a ? !0 : Gv(n, a.parentElement) : !1, fd = (n)=>n.pointerType === "mouse" ? typeof n.button != "number" || n.button <= 0 : n.isPrimary !== !1, a2 = new Set([
        "BUTTON",
        "INPUT",
        "SELECT",
        "TEXTAREA",
        "A"
    ]);
    function i2(n) {
        return a2.has(n.tagName) || n.isContentEditable === !0;
    }
    const l2 = new Set([
        "INPUT",
        "SELECT",
        "TEXTAREA"
    ]);
    function s2(n) {
        return l2.has(n.tagName) || n.isContentEditable === !0;
    }
    const Nr = new WeakSet;
    function ly(n) {
        return (a)=>{
            a.key === "Enter" && n(a);
        };
    }
    function qc(n, a) {
        n.dispatchEvent(new PointerEvent("pointer" + a, {
            isPrimary: !0,
            bubbles: !0
        }));
    }
    const r2 = (n, a)=>{
        const l = n.currentTarget;
        if (!l) return;
        const r = ly(()=>{
            if (Nr.has(l)) return;
            qc(l, "down");
            const u = ly(()=>{
                qc(l, "up");
            }), d = ()=>qc(l, "cancel");
            l.addEventListener("keyup", u, a), l.addEventListener("blur", d, a);
        });
        l.addEventListener("keydown", r, a), l.addEventListener("blur", ()=>l.removeEventListener("keydown", r), a);
    };
    function sy(n) {
        return fd(n) && !Uv();
    }
    const ry = new WeakSet;
    function o2(n, a, l = {}) {
        const [r, u, d] = Hv(n, l), f = (h)=>{
            const m = h.currentTarget;
            if (!sy(h) || ry.has(h)) return;
            Nr.add(m), l.stopPropagation && ry.add(h);
            const p = a(m, h), y = (A, E)=>{
                window.removeEventListener("pointerup", v), window.removeEventListener("pointercancel", x), Nr.has(m) && Nr.delete(m), sy(A) && typeof p == "function" && p(A, {
                    success: E
                });
            }, v = (A)=>{
                y(A, m === window || m === document || l.useGlobalTarget || Gv(m, A.target));
            }, x = (A)=>{
                y(A, !1);
            };
            window.addEventListener("pointerup", v, u), window.addEventListener("pointercancel", x, u);
        };
        return r.forEach((h)=>{
            (l.useGlobalTarget ? window : h).addEventListener("pointerdown", f, u), Ef(h) && (h.addEventListener("focus", (p)=>r2(p, u)), !i2(h) && !h.hasAttribute("tabindex") && (h.tabIndex = 0));
        }), d;
    }
    function Ir(n) {
        return G0(n) && "ownerSVGElement" in n;
    }
    const zr = new WeakMap;
    let Lr;
    const qv = (n, a, l)=>(r, u)=>u && u[0] ? u[0][n + "Size"] : Ir(r) && "getBBox" in r ? r.getBBox()[a] : r[l], u2 = qv("inline", "width", "offsetWidth"), c2 = qv("block", "height", "offsetHeight");
    function f2({ target: n, borderBoxSize: a }) {
        zr.get(n)?.forEach((l)=>{
            l(n, {
                get width () {
                    return u2(n, a);
                },
                get height () {
                    return c2(n, a);
                }
            });
        });
    }
    function d2(n) {
        n.forEach(f2);
    }
    function h2() {
        typeof ResizeObserver > "u" || (Lr = new ResizeObserver(d2));
    }
    function m2(n, a) {
        Lr || h2();
        const l = ud(n);
        return l.forEach((r)=>{
            let u = zr.get(r);
            u || (u = new Set, zr.set(r, u)), u.add(a), Lr?.observe(r);
        }), ()=>{
            l.forEach((r)=>{
                const u = zr.get(r);
                u?.delete(a), u?.size || Lr?.unobserve(r);
            });
        };
    }
    const Vr = new Set;
    let Ei;
    function p2() {
        Ei = ()=>{
            const n = {
                get width () {
                    return window.innerWidth;
                },
                get height () {
                    return window.innerHeight;
                }
            };
            Vr.forEach((a)=>a(n));
        }, window.addEventListener("resize", Ei);
    }
    function g2(n) {
        return Vr.add(n), Ei || p2(), ()=>{
            Vr.delete(n), !Vr.size && typeof Ei == "function" && (window.removeEventListener("resize", Ei), Ei = void 0);
        };
    }
    function oy(n, a) {
        return typeof n == "function" ? g2(n) : m2(n, a);
    }
    function kv(n) {
        return Ir(n) && n.tagName === "svg";
    }
    function y2(...n) {
        const a = !Array.isArray(n[0]), l = a ? 0 : -1, r = n[0 + l], u = n[1 + l], d = n[2 + l], f = n[3 + l], h = pv(u, d, f);
        return a ? h(r) : h;
    }
    const v2 = [
        ...zv,
        Wt,
        Ie
    ], b2 = (n)=>v2.find(Nv(n)), uy = ()=>({
            translate: 0,
            scale: 1,
            origin: 0,
            originPoint: 0
        }), Ai = ()=>({
            x: uy(),
            y: uy()
        }), cy = ()=>({
            min: 0,
            max: 0
        }), Jt = ()=>({
            x: cy(),
            y: cy()
        }), Pl = new WeakMap;
    function to(n) {
        return n !== null && typeof n == "object" && typeof n.start == "function";
    }
    function Zl(n) {
        return typeof n == "string" || Array.isArray(n);
    }
    const dd = [
        "animate",
        "whileInView",
        "whileFocus",
        "whileHover",
        "whileTap",
        "whileDrag",
        "exit"
    ], hd = [
        "initial",
        ...dd
    ];
    function eo(n) {
        return to(n.animate) || hd.some((a)=>Zl(n[a]));
    }
    function Yv(n) {
        return !!(eo(n) || n.variants);
    }
    function x2(n, a, l) {
        for(const r in a){
            const u = a[r], d = l[r];
            if (ie(u)) n.addValue(r, u);
            else if (ie(d)) n.addValue(r, oa(u, {
                owner: n
            }));
            else if (d !== u) if (n.hasValue(r)) {
                const f = n.getValue(r);
                f.liveStyle === !0 ? f.jump(u) : f.hasAnimated || f.set(u);
            } else {
                const f = n.getStaticValue(r);
                n.addValue(r, oa(f !== void 0 ? f : u, {
                    owner: n
                }));
            }
        }
        for(const r in l)a[r] === void 0 && n.removeValue(r);
        return a;
    }
    const Af = {
        current: null
    }, Xv = {
        current: !1
    }, S2 = typeof window < "u";
    function T2() {
        if (Xv.current = !0, !!S2) if (window.matchMedia) {
            const n = window.matchMedia("(prefers-reduced-motion)"), a = ()=>Af.current = n.matches;
            n.addEventListener("change", a), a();
        } else Af.current = !1;
    }
    const fy = [
        "AnimationStart",
        "AnimationComplete",
        "Update",
        "BeforeLayoutMeasure",
        "LayoutMeasure",
        "LayoutAnimationStart",
        "LayoutAnimationComplete"
    ];
    let Xr = {};
    function Kv(n) {
        Xr = n;
    }
    function E2() {
        return Xr;
    }
    class Pv {
        scrapeMotionValuesFromProps(a, l, r) {
            return {};
        }
        constructor({ parent: a, props: l, presenceContext: r, reducedMotionConfig: u, skipAnimations: d, blockInitialAnimation: f, visualState: h }, m = {}){
            this.current = null, this.children = new Set, this.isVariantNode = !1, this.isControllingVariants = !1, this.shouldReduceMotion = null, this.shouldSkipAnimations = !1, this.values = new Map, this.KeyframeResolver = ed, this.features = {}, this.valueSubscriptions = new Map, this.prevMotionValues = {}, this.hasBeenMounted = !1, this.events = {}, this.propEventSubscriptions = {}, this.notifyUpdate = ()=>this.notify("Update", this.latestValues), this.render = ()=>{
                this.current && (this.triggerBuild(), this.renderInstance(this.current, this.renderState, this.props.style, this.projection));
            }, this.renderScheduledAt = 0, this.scheduleRender = ()=>{
                const A = me.now();
                this.renderScheduledAt < A && (this.renderScheduledAt = A, Ot.render(this.render, !1, !0));
            };
            const { latestValues: p, renderState: y } = h;
            this.latestValues = p, this.baseTarget = {
                ...p
            }, this.initialValues = l.initial ? {
                ...p
            } : {}, this.renderState = y, this.parent = a, this.props = l, this.presenceContext = r, this.depth = a ? a.depth + 1 : 0, this.reducedMotionConfig = u, this.skipAnimationsConfig = d, this.options = m, this.blockInitialAnimation = !!f, this.isControllingVariants = eo(l), this.isVariantNode = Yv(l), this.isVariantNode && (this.variantChildren = new Set), this.manuallyAnimateOnMount = !!(a && a.current);
            const { willChange: v, ...x } = this.scrapeMotionValuesFromProps(l, {}, this);
            for(const A in x){
                const E = x[A];
                p[A] !== void 0 && ie(E) && E.set(p[A]);
            }
        }
        mount(a) {
            if (this.hasBeenMounted) for(const l in this.initialValues)this.values.get(l)?.jump(this.initialValues[l]), this.latestValues[l] = this.initialValues[l];
            this.current = a, Pl.set(a, this), this.projection && !this.projection.instance && this.projection.mount(a), this.parent && this.isVariantNode && !this.isControllingVariants && (this.removeFromVariantTree = this.parent.addVariantChild(this)), this.values.forEach((l, r)=>this.bindToMotionValue(r, l)), this.reducedMotionConfig === "never" ? this.shouldReduceMotion = !1 : this.reducedMotionConfig === "always" ? this.shouldReduceMotion = !0 : (Xv.current || T2(), this.shouldReduceMotion = Af.current), this.shouldSkipAnimations = this.skipAnimationsConfig ?? !1, this.parent?.addChild(this), this.update(this.props, this.presenceContext), this.hasBeenMounted = !0;
        }
        unmount() {
            this.projection && this.projection.unmount(), jn(this.notifyUpdate), jn(this.render), this.valueSubscriptions.forEach((a)=>a()), this.valueSubscriptions.clear(), this.removeFromVariantTree && this.removeFromVariantTree(), this.parent?.removeChild(this);
            for(const a in this.events)this.events[a].clear();
            for(const a in this.features){
                const l = this.features[a];
                l && (l.unmount(), l.isMounted = !1);
            }
            this.current = null;
        }
        addChild(a) {
            this.children.add(a), this.enteringChildren ?? (this.enteringChildren = new Set), this.enteringChildren.add(a);
        }
        removeChild(a) {
            this.children.delete(a), this.enteringChildren && this.enteringChildren.delete(a);
        }
        bindToMotionValue(a, l) {
            if (this.valueSubscriptions.has(a) && this.valueSubscriptions.get(a)(), l.accelerate && IA.has(a) && this.current instanceof HTMLElement) {
                const { factory: f, keyframes: h, times: m, ease: p, duration: y } = l.accelerate, v = new Cv({
                    element: this.current,
                    name: a,
                    keyframes: h,
                    times: m,
                    ease: p,
                    duration: Pe(y)
                }), x = f(v);
                this.valueSubscriptions.set(a, ()=>{
                    x(), v.cancel();
                });
                return;
            }
            const r = zi.has(a);
            r && this.onBindTransform && this.onBindTransform();
            const u = l.on("change", (f)=>{
                this.latestValues[a] = f, this.props.onUpdate && Ot.preRender(this.notifyUpdate), r && this.projection && (this.projection.isTransformDirty = !0), this.scheduleRender();
            });
            let d;
            typeof window < "u" && window.MotionCheckAppearSync && (d = window.MotionCheckAppearSync(this, a, l)), this.valueSubscriptions.set(a, ()=>{
                u(), d && d(), l.owner && l.stop();
            });
        }
        sortNodePosition(a) {
            return !this.current || !this.sortInstanceNodePosition || this.type !== a.type ? 0 : this.sortInstanceNodePosition(this.current, a.current);
        }
        updateFeatures() {
            let a = "animation";
            for(a in Xr){
                const l = Xr[a];
                if (!l) continue;
                const { isEnabled: r, Feature: u } = l;
                if (!this.features[a] && u && r(this.props) && (this.features[a] = new u(this)), this.features[a]) {
                    const d = this.features[a];
                    d.isMounted ? d.update() : (d.mount(), d.isMounted = !0);
                }
            }
        }
        triggerBuild() {
            this.build(this.renderState, this.latestValues, this.props);
        }
        measureViewportBox() {
            return this.current ? this.measureInstanceViewportBox(this.current, this.props) : Jt();
        }
        getStaticValue(a) {
            return this.latestValues[a];
        }
        setStaticValue(a, l) {
            this.latestValues[a] = l;
        }
        update(a, l) {
            (a.transformTemplate || this.props.transformTemplate) && this.scheduleRender(), this.prevProps = this.props, this.props = a, this.prevPresenceContext = this.presenceContext, this.presenceContext = l;
            for(let r = 0; r < fy.length; r++){
                const u = fy[r];
                this.propEventSubscriptions[u] && (this.propEventSubscriptions[u](), delete this.propEventSubscriptions[u]);
                const d = "on" + u, f = a[d];
                f && (this.propEventSubscriptions[u] = this.on(u, f));
            }
            this.prevMotionValues = x2(this, this.scrapeMotionValuesFromProps(a, this.prevProps || {}, this), this.prevMotionValues), this.handleChildMotionValue && this.handleChildMotionValue();
        }
        getProps() {
            return this.props;
        }
        getVariant(a) {
            return this.props.variants ? this.props.variants[a] : void 0;
        }
        getDefaultTransition() {
            return this.props.transition;
        }
        getTransformPagePoint() {
            return this.props.transformPagePoint;
        }
        getClosestVariantNode() {
            return this.isVariantNode ? this : this.parent ? this.parent.getClosestVariantNode() : void 0;
        }
        addVariantChild(a) {
            const l = this.getClosestVariantNode();
            if (l) return l.variantChildren && l.variantChildren.add(a), ()=>l.variantChildren.delete(a);
        }
        addValue(a, l) {
            const r = this.values.get(a);
            l !== r && (r && this.removeValue(a), this.bindToMotionValue(a, l), this.values.set(a, l), this.latestValues[a] = l.get());
        }
        removeValue(a) {
            this.values.delete(a);
            const l = this.valueSubscriptions.get(a);
            l && (l(), this.valueSubscriptions.delete(a)), delete this.latestValues[a], this.removeValueFromRenderState(a, this.renderState);
        }
        hasValue(a) {
            return this.values.has(a);
        }
        getValue(a, l) {
            if (this.props.values && this.props.values[a]) return this.props.values[a];
            let r = this.values.get(a);
            return r === void 0 && l !== void 0 && (r = oa(l === null ? void 0 : l, {
                owner: this
            }), this.addValue(a, r)), r;
        }
        readValue(a, l) {
            let r = this.latestValues[a] !== void 0 || !this.current ? this.latestValues[a] : this.getBaseTargetFromProps(this.props, a) ?? this.readValueFromInstance(this.current, a, this.options);
            return r != null && (typeof r == "string" && (H0(r) || q0(r)) ? r = parseFloat(r) : !b2(r) && Ie.test(l) && (r = Vv(a, l)), this.setBaseTarget(a, ie(r) ? r.get() : r)), ie(r) ? r.get() : r;
        }
        setBaseTarget(a, l) {
            this.baseTarget[a] = l;
        }
        getBaseTarget(a) {
            const { initial: l } = this.props;
            let r;
            if (typeof l == "string" || typeof l == "object") {
                const d = ld(this.props, l, this.presenceContext?.custom);
                d && (r = d[a]);
            }
            if (l && r !== void 0) return r;
            const u = this.getBaseTargetFromProps(this.props, a);
            return u !== void 0 && !ie(u) ? u : this.initialValues[a] !== void 0 && r === void 0 ? void 0 : this.baseTarget[a];
        }
        on(a, l) {
            return this.events[a] || (this.events[a] = new Xf), this.events[a].add(l);
        }
        notify(a, ...l) {
            this.events[a] && this.events[a].notify(...l);
        }
        scheduleRenderMicrotask() {
            cd.render(this.render);
        }
    }
    class Zv extends Pv {
        constructor(){
            super(...arguments), this.KeyframeResolver = WA;
        }
        sortInstanceNodePosition(a, l) {
            return a.compareDocumentPosition(l) & 2 ? 1 : -1;
        }
        getBaseTargetFromProps(a, l) {
            const r = a.style;
            return r ? r[l] : void 0;
        }
        removeValueFromRenderState(a, { vars: l, style: r }) {
            delete l[a], delete r[a];
        }
        handleChildMotionValue() {
            this.childSubscription && (this.childSubscription(), delete this.childSubscription);
            const { children: a } = this.props;
            ie(a) && (this.childSubscription = a.on("change", (l)=>{
                this.current && (this.current.textContent = `${l}`);
            }));
        }
    }
    class ca {
        constructor(a){
            this.isMounted = !1, this.node = a;
        }
        update() {}
    }
    function Qv({ top: n, left: a, right: l, bottom: r }) {
        return {
            x: {
                min: a,
                max: l
            },
            y: {
                min: n,
                max: r
            }
        };
    }
    function A2({ x: n, y: a }) {
        return {
            top: a.min,
            right: n.max,
            bottom: a.max,
            left: n.min
        };
    }
    function C2(n, a) {
        if (!a) return n;
        const l = a({
            x: n.left,
            y: n.top
        }), r = a({
            x: n.right,
            y: n.bottom
        });
        return {
            top: l.y,
            left: l.x,
            bottom: r.y,
            right: r.x
        };
    }
    function kc(n) {
        return n === void 0 || n === 1;
    }
    function Cf({ scale: n, scaleX: a, scaleY: l }) {
        return !kc(n) || !kc(a) || !kc(l);
    }
    function ja(n) {
        return Cf(n) || Fv(n) || n.z || n.rotate || n.rotateX || n.rotateY || n.skewX || n.skewY;
    }
    function Fv(n) {
        return dy(n.x) || dy(n.y);
    }
    function dy(n) {
        return n && n !== "0%";
    }
    function Kr(n, a, l) {
        const r = n - l, u = a * r;
        return l + u;
    }
    function hy(n, a, l, r, u) {
        return u !== void 0 && (n = Kr(n, u, r)), Kr(n, l, r) + a;
    }
    function wf(n, a = 0, l = 1, r, u) {
        n.min = hy(n.min, a, l, r, u), n.max = hy(n.max, a, l, r, u);
    }
    function $v(n, { x: a, y: l }) {
        wf(n.x, a.translate, a.scale, a.originPoint), wf(n.y, l.translate, l.scale, l.originPoint);
    }
    const my = .999999999999, py = 1.0000000000001;
    function w2(n, a, l, r = !1) {
        const u = l.length;
        if (!u) return;
        a.x = a.y = 1;
        let d, f;
        for(let h = 0; h < u; h++){
            d = l[h], f = d.projectionDelta;
            const { visualElement: m } = d.options;
            m && m.props.style && m.props.style.display === "contents" || (r && d.options.layoutScroll && d.scroll && d !== d.root && wi(n, {
                x: -d.scroll.offset.x,
                y: -d.scroll.offset.y
            }), f && (a.x *= f.x.scale, a.y *= f.y.scale, $v(n, f)), r && ja(d.latestValues) && wi(n, d.latestValues));
        }
        a.x < py && a.x > my && (a.x = 1), a.y < py && a.y > my && (a.y = 1);
    }
    function Ci(n, a) {
        n.min = n.min + a, n.max = n.max + a;
    }
    function gy(n, a, l, r, u = .5) {
        const d = Ut(n.min, n.max, u);
        wf(n, a, l, d, r);
    }
    function yy(n, a) {
        return typeof n == "string" ? parseFloat(n) / 100 * (a.max - a.min) : n;
    }
    function wi(n, a) {
        gy(n.x, yy(a.x, n.x), a.scaleX, a.scale, a.originX), gy(n.y, yy(a.y, n.y), a.scaleY, a.scale, a.originY);
    }
    function Jv(n, a) {
        return Qv(C2(n.getBoundingClientRect(), a));
    }
    function _2(n, a, l) {
        const r = Jv(n, l), { scroll: u } = a;
        return u && (Ci(r.x, u.offset.x), Ci(r.y, u.offset.y)), r;
    }
    const R2 = {
        x: "translateX",
        y: "translateY",
        z: "translateZ",
        transformPerspective: "perspective"
    }, M2 = Ni.length;
    function D2(n, a, l) {
        let r = "", u = !0;
        for(let d = 0; d < M2; d++){
            const f = Ni[d], h = n[f];
            if (h === void 0) continue;
            let m = !0;
            if (typeof h == "number") m = h === (f.startsWith("scale") ? 1 : 0);
            else {
                const p = parseFloat(h);
                m = f.startsWith("scale") ? p === 1 : p === 0;
            }
            if (!m || l) {
                const p = Bv(h, od[f]);
                if (!m) {
                    u = !1;
                    const y = R2[f] || f;
                    r += `${y}(${p}) `;
                }
                l && (a[f] = p);
            }
        }
        return r = r.trim(), l ? r = l(a, u ? "" : r) : u && (r = "none"), r;
    }
    function md(n, a, l) {
        const { style: r, vars: u, transformOrigin: d } = n;
        let f = !1, h = !1;
        for(const m in a){
            const p = a[m];
            if (zi.has(m)) {
                f = !0;
                continue;
            } else if (iv(m)) {
                u[m] = p;
                continue;
            } else {
                const y = Bv(p, od[m]);
                m.startsWith("origin") ? (h = !0, d[m] = y) : r[m] = y;
            }
        }
        if (a.transform || (f || l ? r.transform = D2(a, n.transform, l) : r.transform && (r.transform = "none")), h) {
            const { originX: m = "50%", originY: p = "50%", originZ: y = 0 } = d;
            r.transformOrigin = `${m} ${p} ${y}`;
        }
    }
    function Wv(n, { style: a, vars: l }, r, u) {
        const d = n.style;
        let f;
        for(f in a)d[f] = a[f];
        u?.applyProjectionStyles(d, r);
        for(f in l)d.setProperty(f, l[f]);
    }
    function vy(n, a) {
        return a.max === a.min ? 0 : n / (a.max - a.min) * 100;
    }
    const jl = {
        correct: (n, a)=>{
            if (!a.target) return n;
            if (typeof n == "string") if (tt.test(n)) n = parseFloat(n);
            else return n;
            const l = vy(n, a.target.x), r = vy(n, a.target.y);
            return `${l}% ${r}%`;
        }
    }, j2 = {
        correct: (n, { treeScale: a, projectionDelta: l })=>{
            const r = n, u = Ie.parse(n);
            if (u.length > 5) return r;
            const d = Ie.createTransformer(n), f = typeof u[0] != "number" ? 1 : 0, h = l.x.scale * a.x, m = l.y.scale * a.y;
            u[0 + f] /= h, u[1 + f] /= m;
            const p = Ut(h, m, .5);
            return typeof u[2 + f] == "number" && (u[2 + f] /= p), typeof u[3 + f] == "number" && (u[3 + f] /= p), d(u);
        }
    }, _f = {
        borderRadius: {
            ...jl,
            applyTo: [
                "borderTopLeftRadius",
                "borderTopRightRadius",
                "borderBottomLeftRadius",
                "borderBottomRightRadius"
            ]
        },
        borderTopLeftRadius: jl,
        borderTopRightRadius: jl,
        borderBottomLeftRadius: jl,
        borderBottomRightRadius: jl,
        boxShadow: j2
    };
    function Iv(n, { layout: a, layoutId: l }) {
        return zi.has(n) || n.startsWith("origin") || (a || l !== void 0) && (!!_f[n] || n === "opacity");
    }
    function pd(n, a, l) {
        const r = n.style, u = a?.style, d = {};
        if (!r) return d;
        for(const f in r)(ie(r[f]) || u && ie(u[f]) || Iv(f, n) || l?.getValue(f)?.liveStyle !== void 0) && (d[f] = r[f]);
        return d;
    }
    function O2(n) {
        return window.getComputedStyle(n);
    }
    class tb extends Zv {
        constructor(){
            super(...arguments), this.type = "html", this.renderInstance = Wv;
        }
        readValueFromInstance(a, l) {
            if (zi.has(l)) return this.projection?.isProjecting ? df(l) : IE(a, l);
            {
                const r = O2(a), u = (iv(l) ? r.getPropertyValue(l) : r[l]) || 0;
                return typeof u == "string" ? u.trim() : u;
            }
        }
        measureInstanceViewportBox(a, { transformPagePoint: l }) {
            return Jv(a, l);
        }
        build(a, l, r) {
            md(a, l, r.transformTemplate);
        }
        scrapeMotionValuesFromProps(a, l, r) {
            return pd(a, l, r);
        }
    }
    function N2(n, a) {
        return n in a;
    }
    class z2 extends Pv {
        constructor(){
            super(...arguments), this.type = "object";
        }
        readValueFromInstance(a, l) {
            if (N2(l, a)) {
                const r = a[l];
                if (typeof r == "string" || typeof r == "number") return r;
            }
        }
        getBaseTargetFromProps() {}
        removeValueFromRenderState(a, l) {
            delete l.output[a];
        }
        measureInstanceViewportBox() {
            return Jt();
        }
        build(a, l) {
            Object.assign(a.output, l);
        }
        renderInstance(a, { output: l }) {
            Object.assign(a, l);
        }
        sortInstanceNodePosition() {
            return 0;
        }
    }
    const L2 = {
        offset: "stroke-dashoffset",
        array: "stroke-dasharray"
    }, V2 = {
        offset: "strokeDashoffset",
        array: "strokeDasharray"
    };
    function B2(n, a, l = 1, r = 0, u = !0) {
        n.pathLength = 1;
        const d = u ? L2 : V2;
        n[d.offset] = `${-r}`, n[d.array] = `${a} ${l}`;
    }
    const U2 = [
        "offsetDistance",
        "offsetPath",
        "offsetRotate",
        "offsetAnchor"
    ];
    function eb(n, { attrX: a, attrY: l, attrScale: r, pathLength: u, pathSpacing: d = 1, pathOffset: f = 0, ...h }, m, p, y) {
        if (md(n, h, p), m) {
            n.style.viewBox && (n.attrs.viewBox = n.style.viewBox);
            return;
        }
        n.attrs = n.style, n.style = {};
        const { attrs: v, style: x } = n;
        v.transform && (x.transform = v.transform, delete v.transform), (x.transform || v.transformOrigin) && (x.transformOrigin = v.transformOrigin ?? "50% 50%", delete v.transformOrigin), x.transform && (x.transformBox = y?.transformBox ?? "fill-box", delete v.transformBox);
        for (const A of U2)v[A] !== void 0 && (x[A] = v[A], delete v[A]);
        a !== void 0 && (v.x = a), l !== void 0 && (v.y = l), r !== void 0 && (v.scale = r), u !== void 0 && B2(v, u, d, f, !1);
    }
    const nb = new Set([
        "baseFrequency",
        "diffuseConstant",
        "kernelMatrix",
        "kernelUnitLength",
        "keySplines",
        "keyTimes",
        "limitingConeAngle",
        "markerHeight",
        "markerWidth",
        "numOctaves",
        "targetX",
        "targetY",
        "surfaceScale",
        "specularConstant",
        "specularExponent",
        "stdDeviation",
        "tableValues",
        "viewBox",
        "gradientTransform",
        "pathLength",
        "startOffset",
        "textLength",
        "lengthAdjust"
    ]), ab = (n)=>typeof n == "string" && n.toLowerCase() === "svg";
    function H2(n, a, l, r) {
        Wv(n, a, void 0, r);
        for(const u in a.attrs)n.setAttribute(nb.has(u) ? u : sd(u), a.attrs[u]);
    }
    function ib(n, a, l) {
        const r = pd(n, a, l);
        for(const u in n)if (ie(n[u]) || ie(a[u])) {
            const d = Ni.indexOf(u) !== -1 ? "attr" + u.charAt(0).toUpperCase() + u.substring(1) : u;
            r[d] = n[u];
        }
        return r;
    }
    class lb extends Zv {
        constructor(){
            super(...arguments), this.type = "svg", this.isSVGTag = !1, this.measureInstanceViewportBox = Jt;
        }
        getBaseTargetFromProps(a, l) {
            return a[l];
        }
        readValueFromInstance(a, l) {
            if (zi.has(l)) {
                const r = Lv(l);
                return r && r.default || 0;
            }
            return l = nb.has(l) ? l : sd(l), a.getAttribute(l);
        }
        scrapeMotionValuesFromProps(a, l, r) {
            return ib(a, l, r);
        }
        build(a, l, r) {
            eb(a, l, this.isSVGTag, r.transformTemplate, r.style);
        }
        renderInstance(a, l, r, u) {
            H2(a, l, r, u);
        }
        mount(a) {
            this.isSVGTag = ab(a.tagName), super.mount(a);
        }
    }
    const G2 = hd.length;
    function sb(n) {
        if (!n) return;
        if (!n.isControllingVariants) {
            const l = n.parent ? sb(n.parent) || {} : {};
            return n.props.initial !== void 0 && (l.initial = n.props.initial), l;
        }
        const a = {};
        for(let l = 0; l < G2; l++){
            const r = hd[l], u = n.props[r];
            (Zl(u) || u === !1) && (a[r] = u);
        }
        return a;
    }
    function rb(n, a) {
        if (!Array.isArray(a)) return !1;
        const l = a.length;
        if (l !== n.length) return !1;
        for(let r = 0; r < l; r++)if (a[r] !== n[r]) return !1;
        return !0;
    }
    const q2 = [
        ...dd
    ].reverse(), k2 = dd.length;
    function Y2(n) {
        return (a)=>Promise.all(a.map(({ animation: l, options: r })=>qA(n, l, r)));
    }
    function X2(n) {
        let a = Y2(n), l = by(), r = !0, u = !1;
        const d = (p)=>(y, v)=>{
                const x = _i(n, v, p === "exit" ? n.presenceContext?.custom : void 0);
                if (x) {
                    const { transition: A, transitionEnd: E, ...M } = x;
                    y = {
                        ...y,
                        ...M,
                        ...E
                    };
                }
                return y;
            };
        function f(p) {
            a = p(n);
        }
        function h(p) {
            const { props: y } = n, v = sb(n.parent) || {}, x = [], A = new Set;
            let E = {}, M = 1 / 0;
            for(let z = 0; z < k2; z++){
                const B = q2[z], V = l[B], P = y[B] !== void 0 ? y[B] : v[B], U = Zl(P), X = B === p ? V.isActive : null;
                X === !1 && (M = z);
                let H = P === v[B] && P !== y[B] && U;
                if (H && (r || u) && n.manuallyAnimateOnMount && (H = !1), V.protectedKeys = {
                    ...E
                }, !V.isActive && X === null || !P && !V.prevProp || to(P) || typeof P == "boolean") continue;
                if (B === "exit" && V.isActive && X !== !0) {
                    V.prevResolvedValues && (E = {
                        ...E,
                        ...V.prevResolvedValues
                    });
                    continue;
                }
                const Z = K2(V.prevProp, P);
                let Q = Z || B === p && V.isActive && !H && U || z > M && U, it = !1;
                const bt = Array.isArray(P) ? P : [
                    P
                ];
                let gt = bt.reduce(d(B), {});
                X === !1 && (gt = {});
                const { prevResolvedValues: Nt = {} } = V, ee = {
                    ...Nt,
                    ...gt
                }, Vt = ($)=>{
                    Q = !0, A.has($) && (it = !0, A.delete($)), V.needsAnimating[$] = !0;
                    const st = n.getValue($);
                    st && (st.liveStyle = !1);
                };
                for(const $ in ee){
                    const st = gt[$], ft = Nt[$];
                    if (E.hasOwnProperty($)) continue;
                    let w = !1;
                    vf(st) && vf(ft) ? w = !rb(st, ft) : w = st !== ft, w ? st != null ? Vt($) : A.add($) : st !== void 0 && A.has($) ? Vt($) : V.protectedKeys[$] = !0;
                }
                V.prevProp = P, V.prevResolvedValues = gt, V.isActive && (E = {
                    ...E,
                    ...gt
                }), (r || u) && n.blockInitialAnimation && (Q = !1);
                const G = H && Z;
                Q && (!G || it) && x.push(...bt.map(($)=>{
                    const st = {
                        type: B
                    };
                    if (typeof $ == "string" && (r || u) && !G && n.manuallyAnimateOnMount && n.parent) {
                        const { parent: ft } = n, w = _i(ft, $);
                        if (ft.enteringChildren && w) {
                            const { delayChildren: Y } = w.transition || {};
                            st.delay = _v(ft.enteringChildren, n, Y);
                        }
                    }
                    return {
                        animation: $,
                        options: st
                    };
                }));
            }
            if (A.size) {
                const z = {};
                if (typeof y.initial != "boolean") {
                    const B = _i(n, Array.isArray(y.initial) ? y.initial[0] : y.initial);
                    B && B.transition && (z.transition = B.transition);
                }
                A.forEach((B)=>{
                    const V = n.getBaseTarget(B), P = n.getValue(B);
                    P && (P.liveStyle = !0), z[B] = V ?? null;
                }), x.push({
                    animation: z
                });
            }
            let R = !!x.length;
            return r && (y.initial === !1 || y.initial === y.animate) && !n.manuallyAnimateOnMount && (R = !1), r = !1, u = !1, R ? a(x) : Promise.resolve();
        }
        function m(p, y) {
            if (l[p].isActive === y) return Promise.resolve();
            n.variantChildren?.forEach((x)=>x.animationState?.setActive(p, y)), l[p].isActive = y;
            const v = h(p);
            for(const x in l)l[x].protectedKeys = {};
            return v;
        }
        return {
            animateChanges: h,
            setActive: m,
            setAnimateFunction: f,
            getState: ()=>l,
            reset: ()=>{
                l = by(), u = !0;
            }
        };
    }
    function K2(n, a) {
        return typeof a == "string" ? a !== n : Array.isArray(a) ? !rb(a, n) : !1;
    }
    function Da(n = !1) {
        return {
            isActive: n,
            protectedKeys: {},
            needsAnimating: {},
            prevResolvedValues: {}
        };
    }
    function by() {
        return {
            animate: Da(!0),
            whileInView: Da(),
            whileHover: Da(),
            whileTap: Da(),
            whileDrag: Da(),
            whileFocus: Da(),
            exit: Da()
        };
    }
    function xy(n, a) {
        n.min = a.min, n.max = a.max;
    }
    function Je(n, a) {
        xy(n.x, a.x), xy(n.y, a.y);
    }
    function Sy(n, a) {
        n.translate = a.translate, n.scale = a.scale, n.originPoint = a.originPoint, n.origin = a.origin;
    }
    const ob = 1e-4, P2 = 1 - ob, Z2 = 1 + ob, ub = .01, Q2 = 0 - ub, F2 = 0 + ub;
    function pe(n) {
        return n.max - n.min;
    }
    function $2(n, a, l) {
        return Math.abs(n - a) <= l;
    }
    function Ty(n, a, l, r = .5) {
        n.origin = r, n.originPoint = Ut(a.min, a.max, n.origin), n.scale = pe(l) / pe(a), n.translate = Ut(l.min, l.max, n.origin) - n.originPoint, (n.scale >= P2 && n.scale <= Z2 || isNaN(n.scale)) && (n.scale = 1), (n.translate >= Q2 && n.translate <= F2 || isNaN(n.translate)) && (n.translate = 0);
    }
    function Ul(n, a, l, r) {
        Ty(n.x, a.x, l.x, r ? r.originX : void 0), Ty(n.y, a.y, l.y, r ? r.originY : void 0);
    }
    function Ey(n, a, l) {
        n.min = l.min + a.min, n.max = n.min + pe(a);
    }
    function J2(n, a, l) {
        Ey(n.x, a.x, l.x), Ey(n.y, a.y, l.y);
    }
    function Ay(n, a, l) {
        n.min = a.min - l.min, n.max = n.min + pe(a);
    }
    function Pr(n, a, l) {
        Ay(n.x, a.x, l.x), Ay(n.y, a.y, l.y);
    }
    function Cy(n, a, l, r, u) {
        return n -= a, n = Kr(n, 1 / l, r), u !== void 0 && (n = Kr(n, 1 / u, r)), n;
    }
    function W2(n, a = 0, l = 1, r = .5, u, d = n, f = n) {
        if (rn.test(a) && (a = parseFloat(a), a = Ut(f.min, f.max, a / 100) - f.min), typeof a != "number") return;
        let h = Ut(d.min, d.max, r);
        n === d && (h -= a), n.min = Cy(n.min, a, l, h, u), n.max = Cy(n.max, a, l, h, u);
    }
    function wy(n, a, [l, r, u], d, f) {
        W2(n, a[l], a[r], a[u], a.scale, d, f);
    }
    const I2 = [
        "x",
        "scaleX",
        "originX"
    ], tC = [
        "y",
        "scaleY",
        "originY"
    ];
    function _y(n, a, l, r) {
        wy(n.x, a, I2, l ? l.x : void 0, r ? r.x : void 0), wy(n.y, a, tC, l ? l.y : void 0, r ? r.y : void 0);
    }
    function Ry(n) {
        return n.translate === 0 && n.scale === 1;
    }
    function cb(n) {
        return Ry(n.x) && Ry(n.y);
    }
    function My(n, a) {
        return n.min === a.min && n.max === a.max;
    }
    function eC(n, a) {
        return My(n.x, a.x) && My(n.y, a.y);
    }
    function Dy(n, a) {
        return Math.round(n.min) === Math.round(a.min) && Math.round(n.max) === Math.round(a.max);
    }
    function fb(n, a) {
        return Dy(n.x, a.x) && Dy(n.y, a.y);
    }
    function jy(n) {
        return pe(n.x) / pe(n.y);
    }
    function Oy(n, a) {
        return n.translate === a.translate && n.scale === a.scale && n.originPoint === a.originPoint;
    }
    function ln(n) {
        return [
            n("x"),
            n("y")
        ];
    }
    function nC(n, a, l) {
        let r = "";
        const u = n.x.translate / a.x, d = n.y.translate / a.y, f = l?.z || 0;
        if ((u || d || f) && (r = `translate3d(${u}px, ${d}px, ${f}px) `), (a.x !== 1 || a.y !== 1) && (r += `scale(${1 / a.x}, ${1 / a.y}) `), l) {
            const { transformPerspective: p, rotate: y, rotateX: v, rotateY: x, skewX: A, skewY: E } = l;
            p && (r = `perspective(${p}px) ${r}`), y && (r += `rotate(${y}deg) `), v && (r += `rotateX(${v}deg) `), x && (r += `rotateY(${x}deg) `), A && (r += `skewX(${A}deg) `), E && (r += `skewY(${E}deg) `);
        }
        const h = n.x.scale * a.x, m = n.y.scale * a.y;
        return (h !== 1 || m !== 1) && (r += `scale(${h}, ${m})`), r || "none";
    }
    const db = [
        "TopLeft",
        "TopRight",
        "BottomLeft",
        "BottomRight"
    ], aC = db.length, Ny = (n)=>typeof n == "string" ? parseFloat(n) : n, zy = (n)=>typeof n == "number" || tt.test(n);
    function iC(n, a, l, r, u, d) {
        u ? (n.opacity = Ut(0, l.opacity ?? 1, lC(r)), n.opacityExit = Ut(a.opacity ?? 1, 0, sC(r))) : d && (n.opacity = Ut(a.opacity ?? 1, l.opacity ?? 1, r));
        for(let f = 0; f < aC; f++){
            const h = `border${db[f]}Radius`;
            let m = Ly(a, h), p = Ly(l, h);
            if (m === void 0 && p === void 0) continue;
            m || (m = 0), p || (p = 0), m === 0 || p === 0 || zy(m) === zy(p) ? (n[h] = Math.max(Ut(Ny(m), Ny(p), r), 0), (rn.test(p) || rn.test(m)) && (n[h] += "%")) : n[h] = p;
        }
        (a.rotate || l.rotate) && (n.rotate = Ut(a.rotate || 0, l.rotate || 0, r));
    }
    function Ly(n, a) {
        return n[a] !== void 0 ? n[a] : n.borderRadius;
    }
    const lC = hb(0, .5, $0), sC = hb(.5, .95, Ke);
    function hb(n, a, l) {
        return (r)=>r < n ? 0 : r > a ? 1 : l(Mi(n, a, r));
    }
    function mb(n, a, l) {
        const r = ie(n) ? n : oa(n);
        return r.start(id("", r, a, l)), r.animation;
    }
    function Ql(n, a, l, r = {
        passive: !0
    }) {
        return n.addEventListener(a, l, r), ()=>n.removeEventListener(a, l);
    }
    const rC = (n, a)=>n.depth - a.depth;
    class oC {
        constructor(){
            this.children = [], this.isDirty = !1;
        }
        add(a) {
            kf(this.children, a), this.isDirty = !0;
        }
        remove(a) {
            Ri(this.children, a), this.isDirty = !0;
        }
        forEach(a) {
            this.isDirty && this.children.sort(rC), this.isDirty = !1, this.children.forEach(a);
        }
    }
    function uC(n, a) {
        const l = me.now(), r = ({ timestamp: u })=>{
            const d = u - l;
            d >= a && (jn(r), n(d - a));
        };
        return Ot.setup(r, !0), ()=>jn(r);
    }
    function Br(n) {
        return ie(n) ? n.get() : n;
    }
    class cC {
        constructor(){
            this.members = [];
        }
        add(a) {
            kf(this.members, a);
            for(let l = this.members.length - 1; l >= 0; l--){
                const r = this.members[l];
                if (r === a || r === this.lead || r === this.prevLead) continue;
                const u = r.instance;
                (!u || u.isConnected === !1) && !r.snapshot && (Ri(this.members, r), r.unmount());
            }
            a.scheduleRender();
        }
        remove(a) {
            if (Ri(this.members, a), a === this.prevLead && (this.prevLead = void 0), a === this.lead) {
                const l = this.members[this.members.length - 1];
                l && this.promote(l);
            }
        }
        relegate(a) {
            for(let l = this.members.indexOf(a) - 1; l >= 0; l--){
                const r = this.members[l];
                if (r.isPresent !== !1 && r.instance?.isConnected !== !1) return this.promote(r), !0;
            }
            return !1;
        }
        promote(a, l) {
            const r = this.lead;
            if (a !== r && (this.prevLead = r, this.lead = a, a.show(), r)) {
                r.updateSnapshot(), a.scheduleRender();
                const { layoutDependency: u } = r.options, { layoutDependency: d } = a.options;
                (u === void 0 || u !== d) && (a.resumeFrom = r, l && (r.preserveOpacity = !0), r.snapshot && (a.snapshot = r.snapshot, a.snapshot.latestValues = r.animationValues || r.latestValues), a.root?.isUpdating && (a.isLayoutDirty = !0)), a.options.crossfade === !1 && r.hide();
            }
        }
        exitAnimationComplete() {
            this.members.forEach((a)=>{
                a.options.onExitComplete?.(), a.resumingFrom?.options.onExitComplete?.();
            });
        }
        scheduleRender() {
            this.members.forEach((a)=>a.instance && a.scheduleRender(!1));
        }
        removeLeadSnapshot() {
            this.lead?.snapshot && (this.lead.snapshot = void 0);
        }
    }
    const Ur = {
        hasAnimatedSinceResize: !0,
        hasEverUpdated: !1
    }, Yc = [
        "",
        "X",
        "Y",
        "Z"
    ], fC = 1e3;
    let dC = 0;
    function Xc(n, a, l, r) {
        const { latestValues: u } = a;
        u[n] && (l[n] = u[n], a.setStaticValue(n, 0), r && (r[n] = 0));
    }
    function pb(n) {
        if (n.hasCheckedOptimisedAppear = !0, n.root === n) return;
        const { visualElement: a } = n.options;
        if (!a) return;
        const l = Ov(a);
        if (window.MotionHasOptimisedAnimation(l, "transform")) {
            const { layout: u, layoutId: d } = n.options;
            window.MotionCancelOptimisedAnimation(l, "transform", Ot, !(u || d));
        }
        const { parent: r } = n;
        r && !r.hasCheckedOptimisedAppear && pb(r);
    }
    function gb({ attachResizeListener: n, defaultParent: a, measureScroll: l, checkIsScrollRoot: r, resetTransform: u }) {
        return class {
            constructor(f = {}, h = a?.()){
                this.id = dC++, this.animationId = 0, this.animationCommitId = 0, this.children = new Set, this.options = {}, this.isTreeAnimating = !1, this.isAnimationBlocked = !1, this.isLayoutDirty = !1, this.isProjectionDirty = !1, this.isSharedProjectionDirty = !1, this.isTransformDirty = !1, this.updateManuallyBlocked = !1, this.updateBlockedByResize = !1, this.isUpdating = !1, this.isSVG = !1, this.needsReset = !1, this.shouldResetTransform = !1, this.hasCheckedOptimisedAppear = !1, this.treeScale = {
                    x: 1,
                    y: 1
                }, this.eventHandlers = new Map, this.hasTreeAnimated = !1, this.layoutVersion = 0, this.updateScheduled = !1, this.scheduleUpdate = ()=>this.update(), this.projectionUpdateScheduled = !1, this.checkUpdateFailed = ()=>{
                    this.isUpdating && (this.isUpdating = !1, this.clearAllSnapshots());
                }, this.updateProjection = ()=>{
                    this.projectionUpdateScheduled = !1, this.nodes.forEach(pC), this.nodes.forEach(bC), this.nodes.forEach(xC), this.nodes.forEach(gC);
                }, this.resolvedRelativeTargetAt = 0, this.linkedParentVersion = 0, this.hasProjected = !1, this.isVisible = !0, this.animationProgress = 0, this.sharedNodes = new Map, this.latestValues = f, this.root = h ? h.root || h : this, this.path = h ? [
                    ...h.path,
                    h
                ] : [], this.parent = h, this.depth = h ? h.depth + 1 : 0;
                for(let m = 0; m < this.path.length; m++)this.path[m].shouldResetTransform = !0;
                this.root === this && (this.nodes = new oC);
            }
            addEventListener(f, h) {
                return this.eventHandlers.has(f) || this.eventHandlers.set(f, new Xf), this.eventHandlers.get(f).add(h);
            }
            notifyListeners(f, ...h) {
                const m = this.eventHandlers.get(f);
                m && m.notify(...h);
            }
            hasListeners(f) {
                return this.eventHandlers.has(f);
            }
            mount(f) {
                if (this.instance) return;
                this.isSVG = Ir(f) && !kv(f), this.instance = f;
                const { layoutId: h, layout: m, visualElement: p } = this.options;
                if (p && !p.current && p.mount(f), this.root.nodes.add(this), this.parent && this.parent.children.add(this), this.root.hasTreeAnimated && (m || h) && (this.isLayoutDirty = !0), n) {
                    let y, v = 0;
                    const x = ()=>this.root.updateBlockedByResize = !1;
                    Ot.read(()=>{
                        v = window.innerWidth;
                    }), n(f, ()=>{
                        const A = window.innerWidth;
                        A !== v && (v = A, this.root.updateBlockedByResize = !0, y && y(), y = uC(x, 250), Ur.hasAnimatedSinceResize && (Ur.hasAnimatedSinceResize = !1, this.nodes.forEach(Uy)));
                    });
                }
                h && this.root.registerSharedNode(h, this), this.options.animate !== !1 && p && (h || m) && this.addEventListener("didUpdate", ({ delta: y, hasLayoutChanged: v, hasRelativeLayoutChanged: x, layout: A })=>{
                    if (this.isTreeAnimationBlocked()) {
                        this.target = void 0, this.relativeTarget = void 0;
                        return;
                    }
                    const E = this.options.transition || p.getDefaultTransition() || CC, { onLayoutAnimationStart: M, onLayoutAnimationComplete: R } = p.getProps(), z = !this.targetLayout || !fb(this.targetLayout, A), B = !v && x;
                    if (this.options.layoutRoot || this.resumeFrom || B || v && (z || !this.currentAnimation)) {
                        this.resumeFrom && (this.resumingFrom = this.resumeFrom, this.resumingFrom.resumingFrom = void 0);
                        const V = {
                            ...ad(E, "layout"),
                            onPlay: M,
                            onComplete: R
                        };
                        (p.shouldReduceMotion || this.options.layoutRoot) && (V.delay = 0, V.type = !1), this.startAnimation(V), this.setAnimationOrigin(y, B);
                    } else v || Uy(this), this.isLead() && this.options.onExitComplete && this.options.onExitComplete();
                    this.targetLayout = A;
                });
            }
            unmount() {
                this.options.layoutId && this.willUpdate(), this.root.nodes.remove(this);
                const f = this.getStack();
                f && f.remove(this), this.parent && this.parent.children.delete(this), this.instance = void 0, this.eventHandlers.clear(), jn(this.updateProjection);
            }
            blockUpdate() {
                this.updateManuallyBlocked = !0;
            }
            unblockUpdate() {
                this.updateManuallyBlocked = !1;
            }
            isUpdateBlocked() {
                return this.updateManuallyBlocked || this.updateBlockedByResize;
            }
            isTreeAnimationBlocked() {
                return this.isAnimationBlocked || this.parent && this.parent.isTreeAnimationBlocked() || !1;
            }
            startUpdate() {
                this.isUpdateBlocked() || (this.isUpdating = !0, this.nodes && this.nodes.forEach(SC), this.animationId++);
            }
            getTransformTemplate() {
                const { visualElement: f } = this.options;
                return f && f.getProps().transformTemplate;
            }
            willUpdate(f = !0) {
                if (this.root.hasTreeAnimated = !0, this.root.isUpdateBlocked()) {
                    this.options.onExitComplete && this.options.onExitComplete();
                    return;
                }
                if (window.MotionCancelOptimisedAnimation && !this.hasCheckedOptimisedAppear && pb(this), !this.root.isUpdating && this.root.startUpdate(), this.isLayoutDirty) return;
                this.isLayoutDirty = !0;
                for(let y = 0; y < this.path.length; y++){
                    const v = this.path[y];
                    v.shouldResetTransform = !0, v.updateScroll("snapshot"), v.options.layoutRoot && v.willUpdate(!1);
                }
                const { layoutId: h, layout: m } = this.options;
                if (h === void 0 && !m) return;
                const p = this.getTransformTemplate();
                this.prevTransformTemplateValue = p ? p(this.latestValues, "") : void 0, this.updateSnapshot(), f && this.notifyListeners("willUpdate");
            }
            update() {
                if (this.updateScheduled = !1, this.isUpdateBlocked()) {
                    this.unblockUpdate(), this.clearAllSnapshots(), this.nodes.forEach(Vy);
                    return;
                }
                if (this.animationId <= this.animationCommitId) {
                    this.nodes.forEach(By);
                    return;
                }
                this.animationCommitId = this.animationId, this.isUpdating ? (this.isUpdating = !1, this.nodes.forEach(vC), this.nodes.forEach(hC), this.nodes.forEach(mC)) : this.nodes.forEach(By), this.clearAllSnapshots();
                const h = me.now();
                ce.delta = un(0, 1e3 / 60, h - ce.timestamp), ce.timestamp = h, ce.isProcessing = !0, Lc.update.process(ce), Lc.preRender.process(ce), Lc.render.process(ce), ce.isProcessing = !1;
            }
            didUpdate() {
                this.updateScheduled || (this.updateScheduled = !0, cd.read(this.scheduleUpdate));
            }
            clearAllSnapshots() {
                this.nodes.forEach(yC), this.sharedNodes.forEach(TC);
            }
            scheduleUpdateProjection() {
                this.projectionUpdateScheduled || (this.projectionUpdateScheduled = !0, Ot.preRender(this.updateProjection, !1, !0));
            }
            scheduleCheckAfterUnmount() {
                Ot.postRender(()=>{
                    this.isLayoutDirty ? this.root.didUpdate() : this.root.checkUpdateFailed();
                });
            }
            updateSnapshot() {
                this.snapshot || !this.instance || (this.snapshot = this.measure(), this.snapshot && !pe(this.snapshot.measuredBox.x) && !pe(this.snapshot.measuredBox.y) && (this.snapshot = void 0));
            }
            updateLayout() {
                if (!this.instance || (this.updateScroll(), !(this.options.alwaysMeasureLayout && this.isLead()) && !this.isLayoutDirty)) return;
                if (this.resumeFrom && !this.resumeFrom.instance) for(let m = 0; m < this.path.length; m++)this.path[m].updateScroll();
                const f = this.layout;
                this.layout = this.measure(!1), this.layoutVersion++, this.layoutCorrected = Jt(), this.isLayoutDirty = !1, this.projectionDelta = void 0, this.notifyListeners("measure", this.layout.layoutBox);
                const { visualElement: h } = this.options;
                h && h.notify("LayoutMeasure", this.layout.layoutBox, f ? f.layoutBox : void 0);
            }
            updateScroll(f = "measure") {
                let h = !!(this.options.layoutScroll && this.instance);
                if (this.scroll && this.scroll.animationId === this.root.animationId && this.scroll.phase === f && (h = !1), h && this.instance) {
                    const m = r(this.instance);
                    this.scroll = {
                        animationId: this.root.animationId,
                        phase: f,
                        isRoot: m,
                        offset: l(this.instance),
                        wasRoot: this.scroll ? this.scroll.isRoot : m
                    };
                }
            }
            resetTransform() {
                if (!u) return;
                const f = this.isLayoutDirty || this.shouldResetTransform || this.options.alwaysMeasureLayout, h = this.projectionDelta && !cb(this.projectionDelta), m = this.getTransformTemplate(), p = m ? m(this.latestValues, "") : void 0, y = p !== this.prevTransformTemplateValue;
                f && this.instance && (h || ja(this.latestValues) || y) && (u(this.instance, p), this.shouldResetTransform = !1, this.scheduleRender());
            }
            measure(f = !0) {
                const h = this.measurePageBox();
                let m = this.removeElementScroll(h);
                return f && (m = this.removeTransform(m)), wC(m), {
                    animationId: this.root.animationId,
                    measuredBox: h,
                    layoutBox: m,
                    latestValues: {},
                    source: this.id
                };
            }
            measurePageBox() {
                const { visualElement: f } = this.options;
                if (!f) return Jt();
                const h = f.measureViewportBox();
                if (!(this.scroll?.wasRoot || this.path.some(_C))) {
                    const { scroll: p } = this.root;
                    p && (Ci(h.x, p.offset.x), Ci(h.y, p.offset.y));
                }
                return h;
            }
            removeElementScroll(f) {
                const h = Jt();
                if (Je(h, f), this.scroll?.wasRoot) return h;
                for(let m = 0; m < this.path.length; m++){
                    const p = this.path[m], { scroll: y, options: v } = p;
                    p !== this.root && y && v.layoutScroll && (y.wasRoot && Je(h, f), Ci(h.x, y.offset.x), Ci(h.y, y.offset.y));
                }
                return h;
            }
            applyTransform(f, h = !1) {
                const m = Jt();
                Je(m, f);
                for(let p = 0; p < this.path.length; p++){
                    const y = this.path[p];
                    !h && y.options.layoutScroll && y.scroll && y !== y.root && wi(m, {
                        x: -y.scroll.offset.x,
                        y: -y.scroll.offset.y
                    }), ja(y.latestValues) && wi(m, y.latestValues);
                }
                return ja(this.latestValues) && wi(m, this.latestValues), m;
            }
            removeTransform(f) {
                const h = Jt();
                Je(h, f);
                for(let m = 0; m < this.path.length; m++){
                    const p = this.path[m];
                    if (!p.instance || !ja(p.latestValues)) continue;
                    Cf(p.latestValues) && p.updateSnapshot();
                    const y = Jt(), v = p.measurePageBox();
                    Je(y, v), _y(h, p.latestValues, p.snapshot ? p.snapshot.layoutBox : void 0, y);
                }
                return ja(this.latestValues) && _y(h, this.latestValues), h;
            }
            setTargetDelta(f) {
                this.targetDelta = f, this.root.scheduleUpdateProjection(), this.isProjectionDirty = !0;
            }
            setOptions(f) {
                this.options = {
                    ...this.options,
                    ...f,
                    crossfade: f.crossfade !== void 0 ? f.crossfade : !0
                };
            }
            clearMeasurements() {
                this.scroll = void 0, this.layout = void 0, this.snapshot = void 0, this.prevTransformTemplateValue = void 0, this.targetDelta = void 0, this.target = void 0, this.isLayoutDirty = !1;
            }
            forceRelativeParentToResolveTarget() {
                this.relativeParent && this.relativeParent.resolvedRelativeTargetAt !== ce.timestamp && this.relativeParent.resolveTargetDelta(!0);
            }
            resolveTargetDelta(f = !1) {
                const h = this.getLead();
                this.isProjectionDirty || (this.isProjectionDirty = h.isProjectionDirty), this.isTransformDirty || (this.isTransformDirty = h.isTransformDirty), this.isSharedProjectionDirty || (this.isSharedProjectionDirty = h.isSharedProjectionDirty);
                const m = !!this.resumingFrom || this !== h;
                if (!(f || m && this.isSharedProjectionDirty || this.isProjectionDirty || this.parent?.isProjectionDirty || this.attemptToResolveRelativeTarget || this.root.updateBlockedByResize)) return;
                const { layout: y, layoutId: v } = this.options;
                if (!this.layout || !(y || v)) return;
                this.resolvedRelativeTargetAt = ce.timestamp;
                const x = this.getClosestProjectingParent();
                x && this.linkedParentVersion !== x.layoutVersion && !x.options.layoutRoot && this.removeRelativeTarget(), !this.targetDelta && !this.relativeTarget && (x && x.layout ? this.createRelativeTarget(x, this.layout.layoutBox, x.layout.layoutBox) : this.removeRelativeTarget()), !(!this.relativeTarget && !this.targetDelta) && (this.target || (this.target = Jt(), this.targetWithTransforms = Jt()), this.relativeTarget && this.relativeTargetOrigin && this.relativeParent && this.relativeParent.target ? (this.forceRelativeParentToResolveTarget(), J2(this.target, this.relativeTarget, this.relativeParent.target)) : this.targetDelta ? (this.resumingFrom ? this.target = this.applyTransform(this.layout.layoutBox) : Je(this.target, this.layout.layoutBox), $v(this.target, this.targetDelta)) : Je(this.target, this.layout.layoutBox), this.attemptToResolveRelativeTarget && (this.attemptToResolveRelativeTarget = !1, x && !!x.resumingFrom == !!this.resumingFrom && !x.options.layoutScroll && x.target && this.animationProgress !== 1 ? this.createRelativeTarget(x, this.target, x.target) : this.relativeParent = this.relativeTarget = void 0));
            }
            getClosestProjectingParent() {
                if (!(!this.parent || Cf(this.parent.latestValues) || Fv(this.parent.latestValues))) return this.parent.isProjecting() ? this.parent : this.parent.getClosestProjectingParent();
            }
            isProjecting() {
                return !!((this.relativeTarget || this.targetDelta || this.options.layoutRoot) && this.layout);
            }
            createRelativeTarget(f, h, m) {
                this.relativeParent = f, this.linkedParentVersion = f.layoutVersion, this.forceRelativeParentToResolveTarget(), this.relativeTarget = Jt(), this.relativeTargetOrigin = Jt(), Pr(this.relativeTargetOrigin, h, m), Je(this.relativeTarget, this.relativeTargetOrigin);
            }
            removeRelativeTarget() {
                this.relativeParent = this.relativeTarget = void 0;
            }
            calcProjection() {
                const f = this.getLead(), h = !!this.resumingFrom || this !== f;
                let m = !0;
                if ((this.isProjectionDirty || this.parent?.isProjectionDirty) && (m = !1), h && (this.isSharedProjectionDirty || this.isTransformDirty) && (m = !1), this.resolvedRelativeTargetAt === ce.timestamp && (m = !1), m) return;
                const { layout: p, layoutId: y } = this.options;
                if (this.isTreeAnimating = !!(this.parent && this.parent.isTreeAnimating || this.currentAnimation || this.pendingAnimation), this.isTreeAnimating || (this.targetDelta = this.relativeTarget = void 0), !this.layout || !(p || y)) return;
                Je(this.layoutCorrected, this.layout.layoutBox);
                const v = this.treeScale.x, x = this.treeScale.y;
                w2(this.layoutCorrected, this.treeScale, this.path, h), f.layout && !f.target && (this.treeScale.x !== 1 || this.treeScale.y !== 1) && (f.target = f.layout.layoutBox, f.targetWithTransforms = Jt());
                const { target: A } = f;
                if (!A) {
                    this.prevProjectionDelta && (this.createProjectionDeltas(), this.scheduleRender());
                    return;
                }
                !this.projectionDelta || !this.prevProjectionDelta ? this.createProjectionDeltas() : (Sy(this.prevProjectionDelta.x, this.projectionDelta.x), Sy(this.prevProjectionDelta.y, this.projectionDelta.y)), Ul(this.projectionDelta, this.layoutCorrected, A, this.latestValues), (this.treeScale.x !== v || this.treeScale.y !== x || !Oy(this.projectionDelta.x, this.prevProjectionDelta.x) || !Oy(this.projectionDelta.y, this.prevProjectionDelta.y)) && (this.hasProjected = !0, this.scheduleRender(), this.notifyListeners("projectionUpdate", A));
            }
            hide() {
                this.isVisible = !1;
            }
            show() {
                this.isVisible = !0;
            }
            scheduleRender(f = !0) {
                if (this.options.visualElement?.scheduleRender(), f) {
                    const h = this.getStack();
                    h && h.scheduleRender();
                }
                this.resumingFrom && !this.resumingFrom.instance && (this.resumingFrom = void 0);
            }
            createProjectionDeltas() {
                this.prevProjectionDelta = Ai(), this.projectionDelta = Ai(), this.projectionDeltaWithTransform = Ai();
            }
            setAnimationOrigin(f, h = !1) {
                const m = this.snapshot, p = m ? m.latestValues : {}, y = {
                    ...this.latestValues
                }, v = Ai();
                (!this.relativeParent || !this.relativeParent.options.layoutRoot) && (this.relativeTarget = this.relativeTargetOrigin = void 0), this.attemptToResolveRelativeTarget = !h;
                const x = Jt(), A = m ? m.source : void 0, E = this.layout ? this.layout.source : void 0, M = A !== E, R = this.getStack(), z = !R || R.members.length <= 1, B = !!(M && !z && this.options.crossfade === !0 && !this.path.some(AC));
                this.animationProgress = 0;
                let V;
                this.mixTargetDelta = (P)=>{
                    const U = P / 1e3;
                    Hy(v.x, f.x, U), Hy(v.y, f.y, U), this.setTargetDelta(v), this.relativeTarget && this.relativeTargetOrigin && this.layout && this.relativeParent && this.relativeParent.layout && (Pr(x, this.layout.layoutBox, this.relativeParent.layout.layoutBox), EC(this.relativeTarget, this.relativeTargetOrigin, x, U), V && eC(this.relativeTarget, V) && (this.isProjectionDirty = !1), V || (V = Jt()), Je(V, this.relativeTarget)), M && (this.animationValues = y, iC(y, p, this.latestValues, U, B, z)), this.root.scheduleUpdateProjection(), this.scheduleRender(), this.animationProgress = U;
                }, this.mixTargetDelta(this.options.layoutRoot ? 1e3 : 0);
            }
            startAnimation(f) {
                this.notifyListeners("animationStart"), this.currentAnimation?.stop(), this.resumingFrom?.currentAnimation?.stop(), this.pendingAnimation && (jn(this.pendingAnimation), this.pendingAnimation = void 0), this.pendingAnimation = Ot.update(()=>{
                    Ur.hasAnimatedSinceResize = !0, this.motionValue || (this.motionValue = oa(0)), this.motionValue.jump(0, !1), this.currentAnimation = mb(this.motionValue, [
                        0,
                        1e3
                    ], {
                        ...f,
                        velocity: 0,
                        isSync: !0,
                        onUpdate: (h)=>{
                            this.mixTargetDelta(h), f.onUpdate && f.onUpdate(h);
                        },
                        onStop: ()=>{},
                        onComplete: ()=>{
                            f.onComplete && f.onComplete(), this.completeAnimation();
                        }
                    }), this.resumingFrom && (this.resumingFrom.currentAnimation = this.currentAnimation), this.pendingAnimation = void 0;
                });
            }
            completeAnimation() {
                this.resumingFrom && (this.resumingFrom.currentAnimation = void 0, this.resumingFrom.preserveOpacity = void 0);
                const f = this.getStack();
                f && f.exitAnimationComplete(), this.resumingFrom = this.currentAnimation = this.animationValues = void 0, this.notifyListeners("animationComplete");
            }
            finishAnimation() {
                this.currentAnimation && (this.mixTargetDelta && this.mixTargetDelta(fC), this.currentAnimation.stop()), this.completeAnimation();
            }
            applyTransformsToTarget() {
                const f = this.getLead();
                let { targetWithTransforms: h, target: m, layout: p, latestValues: y } = f;
                if (!(!h || !m || !p)) {
                    if (this !== f && this.layout && p && yb(this.options.animationType, this.layout.layoutBox, p.layoutBox)) {
                        m = this.target || Jt();
                        const v = pe(this.layout.layoutBox.x);
                        m.x.min = f.target.x.min, m.x.max = m.x.min + v;
                        const x = pe(this.layout.layoutBox.y);
                        m.y.min = f.target.y.min, m.y.max = m.y.min + x;
                    }
                    Je(h, m), wi(h, y), Ul(this.projectionDeltaWithTransform, this.layoutCorrected, h, y);
                }
            }
            registerSharedNode(f, h) {
                this.sharedNodes.has(f) || this.sharedNodes.set(f, new cC), this.sharedNodes.get(f).add(h);
                const p = h.options.initialPromotionConfig;
                h.promote({
                    transition: p ? p.transition : void 0,
                    preserveFollowOpacity: p && p.shouldPreserveFollowOpacity ? p.shouldPreserveFollowOpacity(h) : void 0
                });
            }
            isLead() {
                const f = this.getStack();
                return f ? f.lead === this : !0;
            }
            getLead() {
                const { layoutId: f } = this.options;
                return f ? this.getStack()?.lead || this : this;
            }
            getPrevLead() {
                const { layoutId: f } = this.options;
                return f ? this.getStack()?.prevLead : void 0;
            }
            getStack() {
                const { layoutId: f } = this.options;
                if (f) return this.root.sharedNodes.get(f);
            }
            promote({ needsReset: f, transition: h, preserveFollowOpacity: m } = {}) {
                const p = this.getStack();
                p && p.promote(this, m), f && (this.projectionDelta = void 0, this.needsReset = !0), h && this.setOptions({
                    transition: h
                });
            }
            relegate() {
                const f = this.getStack();
                return f ? f.relegate(this) : !1;
            }
            resetSkewAndRotation() {
                const { visualElement: f } = this.options;
                if (!f) return;
                let h = !1;
                const { latestValues: m } = f;
                if ((m.z || m.rotate || m.rotateX || m.rotateY || m.rotateZ || m.skewX || m.skewY) && (h = !0), !h) return;
                const p = {};
                m.z && Xc("z", f, p, this.animationValues);
                for(let y = 0; y < Yc.length; y++)Xc(`rotate${Yc[y]}`, f, p, this.animationValues), Xc(`skew${Yc[y]}`, f, p, this.animationValues);
                f.render();
                for(const y in p)f.setStaticValue(y, p[y]), this.animationValues && (this.animationValues[y] = p[y]);
                f.scheduleRender();
            }
            applyProjectionStyles(f, h) {
                if (!this.instance || this.isSVG) return;
                if (!this.isVisible) {
                    f.visibility = "hidden";
                    return;
                }
                const m = this.getTransformTemplate();
                if (this.needsReset) {
                    this.needsReset = !1, f.visibility = "", f.opacity = "", f.pointerEvents = Br(h?.pointerEvents) || "", f.transform = m ? m(this.latestValues, "") : "none";
                    return;
                }
                const p = this.getLead();
                if (!this.projectionDelta || !this.layout || !p.target) {
                    this.options.layoutId && (f.opacity = this.latestValues.opacity !== void 0 ? this.latestValues.opacity : 1, f.pointerEvents = Br(h?.pointerEvents) || ""), this.hasProjected && !ja(this.latestValues) && (f.transform = m ? m({}, "") : "none", this.hasProjected = !1);
                    return;
                }
                f.visibility = "";
                const y = p.animationValues || p.latestValues;
                this.applyTransformsToTarget();
                let v = nC(this.projectionDeltaWithTransform, this.treeScale, y);
                m && (v = m(y, v)), f.transform = v;
                const { x, y: A } = this.projectionDelta;
                f.transformOrigin = `${x.origin * 100}% ${A.origin * 100}% 0`, p.animationValues ? f.opacity = p === this ? y.opacity ?? this.latestValues.opacity ?? 1 : this.preserveOpacity ? this.latestValues.opacity : y.opacityExit : f.opacity = p === this ? y.opacity !== void 0 ? y.opacity : "" : y.opacityExit !== void 0 ? y.opacityExit : 0;
                for(const E in _f){
                    if (y[E] === void 0) continue;
                    const { correct: M, applyTo: R, isCSSVariable: z } = _f[E], B = v === "none" ? y[E] : M(y[E], p);
                    if (R) {
                        const V = R.length;
                        for(let P = 0; P < V; P++)f[R[P]] = B;
                    } else z ? this.options.visualElement.renderState.vars[E] = B : f[E] = B;
                }
                this.options.layoutId && (f.pointerEvents = p === this ? Br(h?.pointerEvents) || "" : "none");
            }
            clearSnapshot() {
                this.resumeFrom = this.snapshot = void 0;
            }
            resetTree() {
                this.root.nodes.forEach((f)=>f.currentAnimation?.stop()), this.root.nodes.forEach(Vy), this.root.sharedNodes.clear();
            }
        };
    }
    function hC(n) {
        n.updateLayout();
    }
    function mC(n) {
        const a = n.resumeFrom?.snapshot || n.snapshot;
        if (n.isLead() && n.layout && a && n.hasListeners("didUpdate")) {
            const { layoutBox: l, measuredBox: r } = n.layout, { animationType: u } = n.options, d = a.source !== n.layout.source;
            u === "size" ? ln((y)=>{
                const v = d ? a.measuredBox[y] : a.layoutBox[y], x = pe(v);
                v.min = l[y].min, v.max = v.min + x;
            }) : yb(u, a.layoutBox, l) && ln((y)=>{
                const v = d ? a.measuredBox[y] : a.layoutBox[y], x = pe(l[y]);
                v.max = v.min + x, n.relativeTarget && !n.currentAnimation && (n.isProjectionDirty = !0, n.relativeTarget[y].max = n.relativeTarget[y].min + x);
            });
            const f = Ai();
            Ul(f, l, a.layoutBox);
            const h = Ai();
            d ? Ul(h, n.applyTransform(r, !0), a.measuredBox) : Ul(h, l, a.layoutBox);
            const m = !cb(f);
            let p = !1;
            if (!n.resumeFrom) {
                const y = n.getClosestProjectingParent();
                if (y && !y.resumeFrom) {
                    const { snapshot: v, layout: x } = y;
                    if (v && x) {
                        const A = Jt();
                        Pr(A, a.layoutBox, v.layoutBox);
                        const E = Jt();
                        Pr(E, l, x.layoutBox), fb(A, E) || (p = !0), y.options.layoutRoot && (n.relativeTarget = E, n.relativeTargetOrigin = A, n.relativeParent = y);
                    }
                }
            }
            n.notifyListeners("didUpdate", {
                layout: l,
                snapshot: a,
                delta: h,
                layoutDelta: f,
                hasLayoutChanged: m,
                hasRelativeLayoutChanged: p
            });
        } else if (n.isLead()) {
            const { onExitComplete: l } = n.options;
            l && l();
        }
        n.options.transition = void 0;
    }
    function pC(n) {
        n.parent && (n.isProjecting() || (n.isProjectionDirty = n.parent.isProjectionDirty), n.isSharedProjectionDirty || (n.isSharedProjectionDirty = !!(n.isProjectionDirty || n.parent.isProjectionDirty || n.parent.isSharedProjectionDirty)), n.isTransformDirty || (n.isTransformDirty = n.parent.isTransformDirty));
    }
    function gC(n) {
        n.isProjectionDirty = n.isSharedProjectionDirty = n.isTransformDirty = !1;
    }
    function yC(n) {
        n.clearSnapshot();
    }
    function Vy(n) {
        n.clearMeasurements();
    }
    function By(n) {
        n.isLayoutDirty = !1;
    }
    function vC(n) {
        const { visualElement: a } = n.options;
        a && a.getProps().onBeforeLayoutMeasure && a.notify("BeforeLayoutMeasure"), n.resetTransform();
    }
    function Uy(n) {
        n.finishAnimation(), n.targetDelta = n.relativeTarget = n.target = void 0, n.isProjectionDirty = !0;
    }
    function bC(n) {
        n.resolveTargetDelta();
    }
    function xC(n) {
        n.calcProjection();
    }
    function SC(n) {
        n.resetSkewAndRotation();
    }
    function TC(n) {
        n.removeLeadSnapshot();
    }
    function Hy(n, a, l) {
        n.translate = Ut(a.translate, 0, l), n.scale = Ut(a.scale, 1, l), n.origin = a.origin, n.originPoint = a.originPoint;
    }
    function Gy(n, a, l, r) {
        n.min = Ut(a.min, l.min, r), n.max = Ut(a.max, l.max, r);
    }
    function EC(n, a, l, r) {
        Gy(n.x, a.x, l.x, r), Gy(n.y, a.y, l.y, r);
    }
    function AC(n) {
        return n.animationValues && n.animationValues.opacityExit !== void 0;
    }
    const CC = {
        duration: .45,
        ease: [
            .4,
            0,
            .1,
            1
        ]
    }, qy = (n)=>typeof navigator < "u" && navigator.userAgent && navigator.userAgent.toLowerCase().includes(n), ky = qy("applewebkit/") && !qy("chrome/") ? Math.round : Ke;
    function Yy(n) {
        n.min = ky(n.min), n.max = ky(n.max);
    }
    function wC(n) {
        Yy(n.x), Yy(n.y);
    }
    function yb(n, a, l) {
        return n === "position" || n === "preserve-aspect" && !$2(jy(a), jy(l), .2);
    }
    function _C(n) {
        return n !== n.root && n.scroll?.wasRoot;
    }
    const RC = gb({
        attachResizeListener: (n, a)=>Ql(n, "resize", a),
        measureScroll: ()=>({
                x: document.documentElement.scrollLeft || document.body?.scrollLeft || 0,
                y: document.documentElement.scrollTop || document.body?.scrollTop || 0
            }),
        checkIsScrollRoot: ()=>!0
    }), Kc = {
        current: void 0
    }, vb = gb({
        measureScroll: (n)=>({
                x: n.scrollLeft,
                y: n.scrollTop
            }),
        defaultParent: ()=>{
            if (!Kc.current) {
                const n = new RC({});
                n.mount(window), n.setOptions({
                    layoutScroll: !0
                }), Kc.current = n;
            }
            return Kc.current;
        },
        resetTransform: (n, a)=>{
            n.style.transform = a !== void 0 ? a : "none";
        },
        checkIsScrollRoot: (n)=>window.getComputedStyle(n).position === "fixed"
    }), no = T.createContext({
        transformPagePoint: (n)=>n,
        isStatic: !1,
        reducedMotion: "never"
    });
    function Xy(n, a) {
        if (typeof n == "function") return n(a);
        n != null && (n.current = a);
    }
    function MC(...n) {
        return (a)=>{
            let l = !1;
            const r = n.map((u)=>{
                const d = Xy(u, a);
                return !l && typeof d == "function" && (l = !0), d;
            });
            if (l) return ()=>{
                for(let u = 0; u < r.length; u++){
                    const d = r[u];
                    typeof d == "function" ? d() : Xy(n[u], null);
                }
            };
        };
    }
    function DC(...n) {
        return T.useCallback(MC(...n), n);
    }
    class jC extends T.Component {
        getSnapshotBeforeUpdate(a) {
            const l = this.props.childRef.current;
            if (l && a.isPresent && !this.props.isPresent && this.props.pop !== !1) {
                const r = l.offsetParent, u = Ef(r) && r.offsetWidth || 0, d = Ef(r) && r.offsetHeight || 0, f = this.props.sizeRef.current;
                f.height = l.offsetHeight || 0, f.width = l.offsetWidth || 0, f.top = l.offsetTop, f.left = l.offsetLeft, f.right = u - f.width - f.left, f.bottom = d - f.height - f.top;
            }
            return null;
        }
        componentDidUpdate() {}
        render() {
            return this.props.children;
        }
    }
    function OC({ children: n, isPresent: a, anchorX: l, anchorY: r, root: u, pop: d }) {
        const f = T.useId(), h = T.useRef(null), m = T.useRef({
            width: 0,
            height: 0,
            top: 0,
            left: 0,
            right: 0,
            bottom: 0
        }), { nonce: p } = T.useContext(no), y = n.props?.ref ?? n?.ref, v = DC(h, y);
        return T.useInsertionEffect(()=>{
            const { width: x, height: A, top: E, left: M, right: R, bottom: z } = m.current;
            if (a || d === !1 || !h.current || !x || !A) return;
            const B = l === "left" ? `left: ${M}` : `right: ${R}`, V = r === "bottom" ? `bottom: ${z}` : `top: ${E}`;
            h.current.dataset.motionPopId = f;
            const P = document.createElement("style");
            p && (P.nonce = p);
            const U = u ?? document.head;
            return U.appendChild(P), P.sheet && P.sheet.insertRule(`
          [data-motion-pop-id="${f}"] {
            position: absolute !important;
            width: ${x}px !important;
            height: ${A}px !important;
            ${B}px !important;
            ${V}px !important;
          }
        `), ()=>{
                U.contains(P) && U.removeChild(P);
            };
        }, [
            a
        ]), S.jsx(jC, {
            isPresent: a,
            childRef: h,
            sizeRef: m,
            pop: d,
            children: d === !1 ? n : T.cloneElement(n, {
                ref: v
            })
        });
    }
    const NC = ({ children: n, initial: a, isPresent: l, onExitComplete: r, custom: u, presenceAffectsLayout: d, mode: f, anchorX: h, anchorY: m, root: p })=>{
        const y = Wl(zC), v = T.useId();
        let x = !0, A = T.useMemo(()=>(x = !1, {
                id: v,
                initial: a,
                isPresent: l,
                custom: u,
                onExitComplete: (E)=>{
                    y.set(E, !0);
                    for (const M of y.values())if (!M) return;
                    r && r();
                },
                register: (E)=>(y.set(E, !1), ()=>y.delete(E))
            }), [
            l,
            y,
            r
        ]);
        return d && x && (A = {
            ...A
        }), T.useMemo(()=>{
            y.forEach((E, M)=>y.set(M, !1));
        }, [
            l
        ]), T.useEffect(()=>{
            !l && !y.size && r && r();
        }, [
            l
        ]), n = S.jsx(OC, {
            pop: f === "popLayout",
            isPresent: l,
            anchorX: h,
            anchorY: m,
            root: p,
            children: n
        }), S.jsx(Wr.Provider, {
            value: A,
            children: n
        });
    };
    function zC() {
        return new Map;
    }
    function bb(n = !0) {
        const a = T.useContext(Wr);
        if (a === null) return [
            !0,
            null
        ];
        const { isPresent: l, onExitComplete: r, register: u } = a, d = T.useId();
        T.useEffect(()=>{
            if (n) return u(d);
        }, [
            n
        ]);
        const f = T.useCallback(()=>n && r && r(d), [
            d,
            r,
            n
        ]);
        return !l && r ? [
            !1,
            f
        ] : [
            !0
        ];
    }
    const Cr = (n)=>n.key || "";
    function Ky(n) {
        const a = [];
        return T.Children.forEach(n, (l)=>{
            T.isValidElement(l) && a.push(l);
        }), a;
    }
    const Nn = ({ children: n, custom: a, initial: l = !0, onExitComplete: r, presenceAffectsLayout: u = !0, mode: d = "sync", propagate: f = !1, anchorX: h = "left", anchorY: m = "top", root: p })=>{
        const [y, v] = bb(f), x = T.useMemo(()=>Ky(n), [
            n
        ]), A = f && !y ? [] : x.map(Cr), E = T.useRef(!0), M = T.useRef(x), R = Wl(()=>new Map), z = T.useRef(new Set), [B, V] = T.useState(x), [P, U] = T.useState(x);
        qf(()=>{
            E.current = !1, M.current = x;
            for(let Z = 0; Z < P.length; Z++){
                const Q = Cr(P[Z]);
                A.includes(Q) ? (R.delete(Q), z.current.delete(Q)) : R.get(Q) !== !0 && R.set(Q, !1);
            }
        }, [
            P,
            A.length,
            A.join("-")
        ]);
        const X = [];
        if (x !== B) {
            let Z = [
                ...x
            ];
            for(let Q = 0; Q < P.length; Q++){
                const it = P[Q], bt = Cr(it);
                A.includes(bt) || (Z.splice(Q, 0, it), X.push(it));
            }
            return d === "wait" && X.length && (Z = X), U(Ky(Z)), V(x), null;
        }
        const { forceRender: H } = T.useContext(Gf);
        return S.jsx(S.Fragment, {
            children: P.map((Z)=>{
                const Q = Cr(Z), it = f && !y ? !1 : x === P || A.includes(Q), bt = ()=>{
                    if (z.current.has(Q)) return;
                    if (z.current.add(Q), R.has(Q)) R.set(Q, !0);
                    else return;
                    let gt = !0;
                    R.forEach((Nt)=>{
                        Nt || (gt = !1);
                    }), gt && (H?.(), U(M.current), f && v?.(), r && r());
                };
                return S.jsx(NC, {
                    isPresent: it,
                    initial: !E.current || l ? void 0 : !1,
                    custom: a,
                    presenceAffectsLayout: u,
                    mode: d,
                    root: p,
                    onExitComplete: it ? void 0 : bt,
                    anchorX: h,
                    anchorY: m,
                    children: Z
                }, Q);
            })
        });
    }, xb = T.createContext({
        strict: !1
    }), Py = {
        animation: [
            "animate",
            "variants",
            "whileHover",
            "whileTap",
            "exit",
            "whileInView",
            "whileFocus",
            "whileDrag"
        ],
        exit: [
            "exit"
        ],
        drag: [
            "drag",
            "dragControls"
        ],
        focus: [
            "whileFocus"
        ],
        hover: [
            "whileHover",
            "onHoverStart",
            "onHoverEnd"
        ],
        tap: [
            "whileTap",
            "onTap",
            "onTapStart",
            "onTapCancel"
        ],
        pan: [
            "onPan",
            "onPanStart",
            "onPanSessionStart",
            "onPanEnd"
        ],
        inView: [
            "whileInView",
            "onViewportEnter",
            "onViewportLeave"
        ],
        layout: [
            "layout",
            "layoutId"
        ]
    };
    let Zy = !1;
    function LC() {
        if (Zy) return;
        const n = {};
        for(const a in Py)n[a] = {
            isEnabled: (l)=>Py[a].some((r)=>!!l[r])
        };
        Kv(n), Zy = !0;
    }
    function Sb() {
        return LC(), E2();
    }
    function VC(n) {
        const a = Sb();
        for(const l in n)a[l] = {
            ...a[l],
            ...n[l]
        };
        Kv(a);
    }
    const BC = new Set([
        "animate",
        "exit",
        "variants",
        "initial",
        "style",
        "values",
        "variants",
        "transition",
        "transformTemplate",
        "custom",
        "inherit",
        "onBeforeLayoutMeasure",
        "onAnimationStart",
        "onAnimationComplete",
        "onUpdate",
        "onDragStart",
        "onDrag",
        "onDragEnd",
        "onMeasureDragConstraints",
        "onDirectionLock",
        "onDragTransitionEnd",
        "_dragX",
        "_dragY",
        "onHoverStart",
        "onHoverEnd",
        "onViewportEnter",
        "onViewportLeave",
        "globalTapTarget",
        "propagate",
        "ignoreStrict",
        "viewport"
    ]);
    function Zr(n) {
        return n.startsWith("while") || n.startsWith("drag") && n !== "draggable" || n.startsWith("layout") || n.startsWith("onTap") || n.startsWith("onPan") || n.startsWith("onLayout") || BC.has(n);
    }
    let Tb = (n)=>!Zr(n);
    function UC(n) {
        typeof n == "function" && (Tb = (a)=>a.startsWith("on") ? !Zr(a) : n(a));
    }
    try {
        UC(require("@emotion/is-prop-valid").default);
    } catch  {}
    function HC(n, a, l) {
        const r = {};
        for(const u in n)u === "values" && typeof n.values == "object" || (Tb(u) || l === !0 && Zr(u) || !a && !Zr(u) || n.draggable && u.startsWith("onDrag")) && (r[u] = n[u]);
        return r;
    }
    const ao = T.createContext({});
    function GC(n, a) {
        if (eo(n)) {
            const { initial: l, animate: r } = n;
            return {
                initial: l === !1 || Zl(l) ? l : void 0,
                animate: Zl(r) ? r : void 0
            };
        }
        return n.inherit !== !1 ? a : {};
    }
    function qC(n) {
        const { initial: a, animate: l } = GC(n, T.useContext(ao));
        return T.useMemo(()=>({
                initial: a,
                animate: l
            }), [
            Qy(a),
            Qy(l)
        ]);
    }
    function Qy(n) {
        return Array.isArray(n) ? n.join(" ") : n;
    }
    const gd = ()=>({
            style: {},
            transform: {},
            transformOrigin: {},
            vars: {}
        });
    function Eb(n, a, l) {
        for(const r in a)!ie(a[r]) && !Iv(r, l) && (n[r] = a[r]);
    }
    function kC({ transformTemplate: n }, a) {
        return T.useMemo(()=>{
            const l = gd();
            return md(l, a, n), Object.assign({}, l.vars, l.style);
        }, [
            a
        ]);
    }
    function YC(n, a) {
        const l = n.style || {}, r = {};
        return Eb(r, l, n), Object.assign(r, kC(n, a)), r;
    }
    function XC(n, a) {
        const l = {}, r = YC(n, a);
        return n.drag && n.dragListener !== !1 && (l.draggable = !1, r.userSelect = r.WebkitUserSelect = r.WebkitTouchCallout = "none", r.touchAction = n.drag === !0 ? "none" : `pan-${n.drag === "x" ? "y" : "x"}`), n.tabIndex === void 0 && (n.onTap || n.onTapStart || n.whileTap) && (l.tabIndex = 0), l.style = r, l;
    }
    const Ab = ()=>({
            ...gd(),
            attrs: {}
        });
    function KC(n, a, l, r) {
        const u = T.useMemo(()=>{
            const d = Ab();
            return eb(d, a, ab(r), n.transformTemplate, n.style), {
                ...d.attrs,
                style: {
                    ...d.style
                }
            };
        }, [
            a
        ]);
        if (n.style) {
            const d = {};
            Eb(d, n.style, n), u.style = {
                ...d,
                ...u.style
            };
        }
        return u;
    }
    const PC = [
        "animate",
        "circle",
        "defs",
        "desc",
        "ellipse",
        "g",
        "image",
        "line",
        "filter",
        "marker",
        "mask",
        "metadata",
        "path",
        "pattern",
        "polygon",
        "polyline",
        "rect",
        "stop",
        "switch",
        "symbol",
        "svg",
        "text",
        "tspan",
        "use",
        "view"
    ];
    function yd(n) {
        return typeof n != "string" || n.includes("-") ? !1 : !!(PC.indexOf(n) > -1 || /[A-Z]/u.test(n));
    }
    function ZC(n, a, l, { latestValues: r }, u, d = !1, f) {
        const m = (f ?? yd(n) ? KC : XC)(a, r, u, n), p = HC(a, typeof n == "string", d), y = n !== T.Fragment ? {
            ...p,
            ...m,
            ref: l
        } : {}, { children: v } = a, x = T.useMemo(()=>ie(v) ? v.get() : v, [
            v
        ]);
        return T.createElement(n, {
            ...y,
            children: x
        });
    }
    function QC({ scrapeMotionValuesFromProps: n, createRenderState: a }, l, r, u) {
        return {
            latestValues: FC(l, r, u, n),
            renderState: a()
        };
    }
    function FC(n, a, l, r) {
        const u = {}, d = r(n, {});
        for(const x in d)u[x] = Br(d[x]);
        let { initial: f, animate: h } = n;
        const m = eo(n), p = Yv(n);
        a && p && !m && n.inherit !== !1 && (f === void 0 && (f = a.initial), h === void 0 && (h = a.animate));
        let y = l ? l.initial === !1 : !1;
        y = y || f === !1;
        const v = y ? h : f;
        if (v && typeof v != "boolean" && !to(v)) {
            const x = Array.isArray(v) ? v : [
                v
            ];
            for(let A = 0; A < x.length; A++){
                const E = ld(n, x[A]);
                if (E) {
                    const { transitionEnd: M, transition: R, ...z } = E;
                    for(const B in z){
                        let V = z[B];
                        if (Array.isArray(V)) {
                            const P = y ? V.length - 1 : 0;
                            V = V[P];
                        }
                        V !== null && (u[B] = V);
                    }
                    for(const B in M)u[B] = M[B];
                }
            }
        }
        return u;
    }
    const Cb = (n)=>(a, l)=>{
            const r = T.useContext(ao), u = T.useContext(Wr), d = ()=>QC(n, a, r, u);
            return l ? d() : Wl(d);
        }, $C = Cb({
        scrapeMotionValuesFromProps: pd,
        createRenderState: gd
    }), JC = Cb({
        scrapeMotionValuesFromProps: ib,
        createRenderState: Ab
    }), WC = Symbol.for("motionComponentSymbol");
    function IC(n, a, l) {
        const r = T.useRef(l);
        T.useInsertionEffect(()=>{
            r.current = l;
        });
        const u = T.useRef(null);
        return T.useCallback((d)=>{
            d && n.onMount?.(d);
            const f = r.current;
            if (typeof f == "function") if (d) {
                const h = f(d);
                typeof h == "function" && (u.current = h);
            } else u.current ? (u.current(), u.current = null) : f(d);
            else f && (f.current = d);
            a && (d ? a.mount(d) : a.unmount());
        }, [
            a
        ]);
    }
    const wb = T.createContext({});
    function Si(n) {
        return n && typeof n == "object" && Object.prototype.hasOwnProperty.call(n, "current");
    }
    function tw(n, a, l, r, u, d) {
        const { visualElement: f } = T.useContext(ao), h = T.useContext(xb), m = T.useContext(Wr), p = T.useContext(no), y = p.reducedMotion, v = p.skipAnimations, x = T.useRef(null), A = T.useRef(!1);
        r = r || h.renderer, !x.current && r && (x.current = r(n, {
            visualState: a,
            parent: f,
            props: l,
            presenceContext: m,
            blockInitialAnimation: m ? m.initial === !1 : !1,
            reducedMotionConfig: y,
            skipAnimations: v,
            isSVG: d
        }), A.current && x.current && (x.current.manuallyAnimateOnMount = !0));
        const E = x.current, M = T.useContext(wb);
        E && !E.projection && u && (E.type === "html" || E.type === "svg") && ew(x.current, l, u, M);
        const R = T.useRef(!1);
        T.useInsertionEffect(()=>{
            E && R.current && E.update(l, m);
        });
        const z = l[jv], B = T.useRef(!!z && typeof window < "u" && !window.MotionHandoffIsComplete?.(z) && window.MotionHasOptimisedAnimation?.(z));
        return qf(()=>{
            A.current = !0, E && (R.current = !0, window.MotionIsMounted = !0, E.updateFeatures(), E.scheduleRenderMicrotask(), B.current && E.animationState && E.animationState.animateChanges());
        }), T.useEffect(()=>{
            E && (!B.current && E.animationState && E.animationState.animateChanges(), B.current && (queueMicrotask(()=>{
                window.MotionHandoffMarkAsComplete?.(z);
            }), B.current = !1), E.enteringChildren = void 0);
        }), E;
    }
    function ew(n, a, l, r) {
        const { layoutId: u, layout: d, drag: f, dragConstraints: h, layoutScroll: m, layoutRoot: p, layoutCrossfade: y } = a;
        n.projection = new l(n.latestValues, a["data-framer-portal-id"] ? void 0 : _b(n.parent)), n.projection.setOptions({
            layoutId: u,
            layout: d,
            alwaysMeasureLayout: !!f || h && Si(h),
            visualElement: n,
            animationType: typeof d == "string" ? d : "both",
            initialPromotionConfig: r,
            crossfade: y,
            layoutScroll: m,
            layoutRoot: p
        });
    }
    function _b(n) {
        if (n) return n.options.allowProjection !== !1 ? n.projection : _b(n.parent);
    }
    function Pc(n, { forwardMotionProps: a = !1, type: l } = {}, r, u) {
        r && VC(r);
        const d = l ? l === "svg" : yd(n), f = d ? JC : $C;
        function h(p, y) {
            let v;
            const x = {
                ...T.useContext(no),
                ...p,
                layoutId: nw(p)
            }, { isStatic: A } = x, E = qC(p), M = f(p, A);
            if (!A && typeof window < "u") {
                aw();
                const R = iw(x);
                v = R.MeasureLayout, E.visualElement = tw(n, M, x, u, R.ProjectionNode, d);
            }
            return S.jsxs(ao.Provider, {
                value: E,
                children: [
                    v && E.visualElement ? S.jsx(v, {
                        visualElement: E.visualElement,
                        ...x
                    }) : null,
                    ZC(n, p, IC(M, E.visualElement, y), M, A, a, d)
                ]
            });
        }
        h.displayName = `motion.${typeof n == "string" ? n : `create(${n.displayName ?? n.name ?? ""})`}`;
        const m = T.forwardRef(h);
        return m[WC] = n, m;
    }
    function nw({ layoutId: n }) {
        const a = T.useContext(Gf).id;
        return a && n !== void 0 ? a + "-" + n : n;
    }
    function aw(n, a) {
        T.useContext(xb).strict;
    }
    function iw(n) {
        const a = Sb(), { drag: l, layout: r } = a;
        if (!l && !r) return {};
        const u = {
            ...l,
            ...r
        };
        return {
            MeasureLayout: l?.isEnabled(n) || r?.isEnabled(n) ? u.MeasureLayout : void 0,
            ProjectionNode: u.ProjectionNode
        };
    }
    function lw(n, a) {
        if (typeof Proxy > "u") return Pc;
        const l = new Map, r = (d, f)=>Pc(d, f, n, a), u = (d, f)=>r(d, f);
        return new Proxy(u, {
            get: (d, f)=>f === "create" ? r : (l.has(f) || l.set(f, Pc(f, void 0, n, a)), l.get(f))
        });
    }
    const sw = (n, a)=>a.isSVG ?? yd(n) ? new lb(a) : new tb(a, {
            allowProjection: n !== T.Fragment
        });
    class rw extends ca {
        constructor(a){
            super(a), a.animationState || (a.animationState = X2(a));
        }
        updateAnimationControlsSubscription() {
            const { animate: a } = this.node.getProps();
            to(a) && (this.unmountControls = a.subscribe(this.node));
        }
        mount() {
            this.updateAnimationControlsSubscription();
        }
        update() {
            const { animate: a } = this.node.getProps(), { animate: l } = this.node.prevProps || {};
            a !== l && this.updateAnimationControlsSubscription();
        }
        unmount() {
            this.node.animationState.reset(), this.unmountControls?.();
        }
    }
    let ow = 0;
    class uw extends ca {
        constructor(){
            super(...arguments), this.id = ow++;
        }
        update() {
            if (!this.node.presenceContext) return;
            const { isPresent: a, onExitComplete: l } = this.node.presenceContext, { isPresent: r } = this.node.prevPresenceContext || {};
            if (!this.node.animationState || a === r) return;
            const u = this.node.animationState.setActive("exit", !a);
            l && !a && u.then(()=>{
                l(this.id);
            });
        }
        mount() {
            const { register: a, onExitComplete: l } = this.node.presenceContext || {};
            l && l(this.id), a && (this.unmount = a(this.id));
        }
        unmount() {}
    }
    const cw = {
        animation: {
            Feature: rw
        },
        exit: {
            Feature: uw
        }
    };
    function ns(n) {
        return {
            point: {
                x: n.pageX,
                y: n.pageY
            }
        };
    }
    const fw = (n)=>(a)=>fd(a) && n(a, ns(a));
    function Hl(n, a, l, r) {
        return Ql(n, a, fw(l), r);
    }
    const Rb = ({ current: n })=>n ? n.ownerDocument.defaultView : null, Fy = (n, a)=>Math.abs(n - a);
    function dw(n, a) {
        const l = Fy(n.x, a.x), r = Fy(n.y, a.y);
        return Math.sqrt(l ** 2 + r ** 2);
    }
    const $y = new Set([
        "auto",
        "scroll"
    ]);
    class Mb {
        constructor(a, l, { transformPagePoint: r, contextWindow: u = window, dragSnapToOrigin: d = !1, distanceThreshold: f = 3, element: h } = {}){
            if (this.startEvent = null, this.lastMoveEvent = null, this.lastMoveEventInfo = null, this.handlers = {}, this.contextWindow = window, this.scrollPositions = new Map, this.removeScrollListeners = null, this.onElementScroll = (A)=>{
                this.handleScroll(A.target);
            }, this.onWindowScroll = ()=>{
                this.handleScroll(window);
            }, this.updatePoint = ()=>{
                if (!(this.lastMoveEvent && this.lastMoveEventInfo)) return;
                const A = Qc(this.lastMoveEventInfo, this.history), E = this.startEvent !== null, M = dw(A.offset, {
                    x: 0,
                    y: 0
                }) >= this.distanceThreshold;
                if (!E && !M) return;
                const { point: R } = A, { timestamp: z } = ce;
                this.history.push({
                    ...R,
                    timestamp: z
                });
                const { onStart: B, onMove: V } = this.handlers;
                E || (B && B(this.lastMoveEvent, A), this.startEvent = this.lastMoveEvent), V && V(this.lastMoveEvent, A);
            }, this.handlePointerMove = (A, E)=>{
                this.lastMoveEvent = A, this.lastMoveEventInfo = Zc(E, this.transformPagePoint), Ot.update(this.updatePoint, !0);
            }, this.handlePointerUp = (A, E)=>{
                this.end();
                const { onEnd: M, onSessionEnd: R, resumeAnimation: z } = this.handlers;
                if ((this.dragSnapToOrigin || !this.startEvent) && z && z(), !(this.lastMoveEvent && this.lastMoveEventInfo)) return;
                const B = Qc(A.type === "pointercancel" ? this.lastMoveEventInfo : Zc(E, this.transformPagePoint), this.history);
                this.startEvent && M && M(A, B), R && R(A, B);
            }, !fd(a)) return;
            this.dragSnapToOrigin = d, this.handlers = l, this.transformPagePoint = r, this.distanceThreshold = f, this.contextWindow = u || window;
            const m = ns(a), p = Zc(m, this.transformPagePoint), { point: y } = p, { timestamp: v } = ce;
            this.history = [
                {
                    ...y,
                    timestamp: v
                }
            ];
            const { onSessionStart: x } = l;
            x && x(a, Qc(p, this.history)), this.removeListeners = Il(Hl(this.contextWindow, "pointermove", this.handlePointerMove), Hl(this.contextWindow, "pointerup", this.handlePointerUp), Hl(this.contextWindow, "pointercancel", this.handlePointerUp)), h && this.startScrollTracking(h);
        }
        startScrollTracking(a) {
            let l = a.parentElement;
            for(; l;){
                const r = getComputedStyle(l);
                ($y.has(r.overflowX) || $y.has(r.overflowY)) && this.scrollPositions.set(l, {
                    x: l.scrollLeft,
                    y: l.scrollTop
                }), l = l.parentElement;
            }
            this.scrollPositions.set(window, {
                x: window.scrollX,
                y: window.scrollY
            }), window.addEventListener("scroll", this.onElementScroll, {
                capture: !0
            }), window.addEventListener("scroll", this.onWindowScroll), this.removeScrollListeners = ()=>{
                window.removeEventListener("scroll", this.onElementScroll, {
                    capture: !0
                }), window.removeEventListener("scroll", this.onWindowScroll);
            };
        }
        handleScroll(a) {
            const l = this.scrollPositions.get(a);
            if (!l) return;
            const r = a === window, u = r ? {
                x: window.scrollX,
                y: window.scrollY
            } : {
                x: a.scrollLeft,
                y: a.scrollTop
            }, d = {
                x: u.x - l.x,
                y: u.y - l.y
            };
            d.x === 0 && d.y === 0 || (r ? this.lastMoveEventInfo && (this.lastMoveEventInfo.point.x += d.x, this.lastMoveEventInfo.point.y += d.y) : this.history.length > 0 && (this.history[0].x -= d.x, this.history[0].y -= d.y), this.scrollPositions.set(a, u), Ot.update(this.updatePoint, !0));
        }
        updateHandlers(a) {
            this.handlers = a;
        }
        end() {
            this.removeListeners && this.removeListeners(), this.removeScrollListeners && this.removeScrollListeners(), this.scrollPositions.clear(), jn(this.updatePoint);
        }
    }
    function Zc(n, a) {
        return a ? {
            point: a(n.point)
        } : n;
    }
    function Jy(n, a) {
        return {
            x: n.x - a.x,
            y: n.y - a.y
        };
    }
    function Qc({ point: n }, a) {
        return {
            point: n,
            delta: Jy(n, Db(a)),
            offset: Jy(n, hw(a)),
            velocity: mw(a, .1)
        };
    }
    function hw(n) {
        return n[0];
    }
    function Db(n) {
        return n[n.length - 1];
    }
    function mw(n, a) {
        if (n.length < 2) return {
            x: 0,
            y: 0
        };
        let l = n.length - 1, r = null;
        const u = Db(n);
        for(; l >= 0 && (r = n[l], !(u.timestamp - r.timestamp > Pe(a)));)l--;
        if (!r) return {
            x: 0,
            y: 0
        };
        r === n[0] && n.length > 2 && u.timestamp - r.timestamp > Pe(a) * 2 && (r = n[1]);
        const d = Xe(u.timestamp - r.timestamp);
        if (d === 0) return {
            x: 0,
            y: 0
        };
        const f = {
            x: (u.x - r.x) / d,
            y: (u.y - r.y) / d
        };
        return f.x === 1 / 0 && (f.x = 0), f.y === 1 / 0 && (f.y = 0), f;
    }
    function pw(n, { min: a, max: l }, r) {
        return a !== void 0 && n < a ? n = r ? Ut(a, n, r.min) : Math.max(n, a) : l !== void 0 && n > l && (n = r ? Ut(l, n, r.max) : Math.min(n, l)), n;
    }
    function Wy(n, a, l) {
        return {
            min: a !== void 0 ? n.min + a : void 0,
            max: l !== void 0 ? n.max + l - (n.max - n.min) : void 0
        };
    }
    function gw(n, { top: a, left: l, bottom: r, right: u }) {
        return {
            x: Wy(n.x, l, u),
            y: Wy(n.y, a, r)
        };
    }
    function Iy(n, a) {
        let l = a.min - n.min, r = a.max - n.max;
        return a.max - a.min < n.max - n.min && ([l, r] = [
            r,
            l
        ]), {
            min: l,
            max: r
        };
    }
    function yw(n, a) {
        return {
            x: Iy(n.x, a.x),
            y: Iy(n.y, a.y)
        };
    }
    function vw(n, a) {
        let l = .5;
        const r = pe(n), u = pe(a);
        return u > r ? l = Mi(a.min, a.max - r, n.min) : r > u && (l = Mi(n.min, n.max - u, a.min)), un(0, 1, l);
    }
    function bw(n, a) {
        const l = {};
        return a.min !== void 0 && (l.min = a.min - n.min), a.max !== void 0 && (l.max = a.max - n.min), l;
    }
    const Rf = .35;
    function xw(n = Rf) {
        return n === !1 ? n = 0 : n === !0 && (n = Rf), {
            x: t0(n, "left", "right"),
            y: t0(n, "top", "bottom")
        };
    }
    function t0(n, a, l) {
        return {
            min: e0(n, a),
            max: e0(n, l)
        };
    }
    function e0(n, a) {
        return typeof n == "number" ? n : n[a] || 0;
    }
    const Sw = new WeakMap;
    class Tw {
        constructor(a){
            this.openDragLock = null, this.isDragging = !1, this.currentDirection = null, this.originPoint = {
                x: 0,
                y: 0
            }, this.constraints = !1, this.hasMutatedConstraints = !1, this.elastic = Jt(), this.latestPointerEvent = null, this.latestPanInfo = null, this.visualElement = a;
        }
        start(a, { snapToCursor: l = !1, distanceThreshold: r } = {}) {
            const { presenceContext: u } = this.visualElement;
            if (u && u.isPresent === !1) return;
            const d = (v)=>{
                l && this.snapToCursor(ns(v).point), this.stopAnimation();
            }, f = (v, x)=>{
                const { drag: A, dragPropagation: E, onDragStart: M } = this.getProps();
                if (A && !E && (this.openDragLock && this.openDragLock(), this.openDragLock = t2(A), !this.openDragLock)) return;
                this.latestPointerEvent = v, this.latestPanInfo = x, this.isDragging = !0, this.currentDirection = null, this.resolveConstraints(), this.visualElement.projection && (this.visualElement.projection.isAnimationBlocked = !0, this.visualElement.projection.target = void 0), ln((z)=>{
                    let B = this.getAxisMotionValue(z).get() || 0;
                    if (rn.test(B)) {
                        const { projection: V } = this.visualElement;
                        if (V && V.layout) {
                            const P = V.layout.layoutBox[z];
                            P && (B = pe(P) * (parseFloat(B) / 100));
                        }
                    }
                    this.originPoint[z] = B;
                }), M && Ot.update(()=>M(v, x), !1, !0), bf(this.visualElement, "transform");
                const { animationState: R } = this.visualElement;
                R && R.setActive("whileDrag", !0);
            }, h = (v, x)=>{
                this.latestPointerEvent = v, this.latestPanInfo = x;
                const { dragPropagation: A, dragDirectionLock: E, onDirectionLock: M, onDrag: R } = this.getProps();
                if (!A && !this.openDragLock) return;
                const { offset: z } = x;
                if (E && this.currentDirection === null) {
                    this.currentDirection = Aw(z), this.currentDirection !== null && M && M(this.currentDirection);
                    return;
                }
                this.updateAxis("x", x.point, z), this.updateAxis("y", x.point, z), this.visualElement.render(), R && Ot.update(()=>R(v, x), !1, !0);
            }, m = (v, x)=>{
                this.latestPointerEvent = v, this.latestPanInfo = x, this.stop(v, x), this.latestPointerEvent = null, this.latestPanInfo = null;
            }, p = ()=>{
                const { dragSnapToOrigin: v } = this.getProps();
                (v || this.constraints) && this.startAnimation({
                    x: 0,
                    y: 0
                });
            }, { dragSnapToOrigin: y } = this.getProps();
            this.panSession = new Mb(a, {
                onSessionStart: d,
                onStart: f,
                onMove: h,
                onSessionEnd: m,
                resumeAnimation: p
            }, {
                transformPagePoint: this.visualElement.getTransformPagePoint(),
                dragSnapToOrigin: y,
                distanceThreshold: r,
                contextWindow: Rb(this.visualElement),
                element: this.visualElement.current
            });
        }
        stop(a, l) {
            const r = a || this.latestPointerEvent, u = l || this.latestPanInfo, d = this.isDragging;
            if (this.cancel(), !d || !u || !r) return;
            const { velocity: f } = u;
            this.startAnimation(f);
            const { onDragEnd: h } = this.getProps();
            h && Ot.postRender(()=>h(r, u));
        }
        cancel() {
            this.isDragging = !1;
            const { projection: a, animationState: l } = this.visualElement;
            a && (a.isAnimationBlocked = !1), this.endPanSession();
            const { dragPropagation: r } = this.getProps();
            !r && this.openDragLock && (this.openDragLock(), this.openDragLock = null), l && l.setActive("whileDrag", !1);
        }
        endPanSession() {
            this.panSession && this.panSession.end(), this.panSession = void 0;
        }
        updateAxis(a, l, r) {
            const { drag: u } = this.getProps();
            if (!r || !wr(a, u, this.currentDirection)) return;
            const d = this.getAxisMotionValue(a);
            let f = this.originPoint[a] + r[a];
            this.constraints && this.constraints[a] && (f = pw(f, this.constraints[a], this.elastic[a])), d.set(f);
        }
        resolveConstraints() {
            const { dragConstraints: a, dragElastic: l } = this.getProps(), r = this.visualElement.projection && !this.visualElement.projection.layout ? this.visualElement.projection.measure(!1) : this.visualElement.projection?.layout, u = this.constraints;
            a && Si(a) ? this.constraints || (this.constraints = this.resolveRefConstraints()) : a && r ? this.constraints = gw(r.layoutBox, a) : this.constraints = !1, this.elastic = xw(l), u !== this.constraints && !Si(a) && r && this.constraints && !this.hasMutatedConstraints && ln((d)=>{
                this.constraints !== !1 && this.getAxisMotionValue(d) && (this.constraints[d] = bw(r.layoutBox[d], this.constraints[d]));
            });
        }
        resolveRefConstraints() {
            const { dragConstraints: a, onMeasureDragConstraints: l } = this.getProps();
            if (!a || !Si(a)) return !1;
            const r = a.current, { projection: u } = this.visualElement;
            if (!u || !u.layout) return !1;
            const d = _2(r, u.root, this.visualElement.getTransformPagePoint());
            let f = yw(u.layout.layoutBox, d);
            if (l) {
                const h = l(A2(f));
                this.hasMutatedConstraints = !!h, h && (f = Qv(h));
            }
            return f;
        }
        startAnimation(a) {
            const { drag: l, dragMomentum: r, dragElastic: u, dragTransition: d, dragSnapToOrigin: f, onDragTransitionEnd: h } = this.getProps(), m = this.constraints || {}, p = ln((y)=>{
                if (!wr(y, l, this.currentDirection)) return;
                let v = m && m[y] || {};
                f && (v = {
                    min: 0,
                    max: 0
                });
                const x = u ? 200 : 1e6, A = u ? 40 : 1e7, E = {
                    type: "inertia",
                    velocity: r ? a[y] : 0,
                    bounceStiffness: x,
                    bounceDamping: A,
                    timeConstant: 750,
                    restDelta: 1,
                    restSpeed: 10,
                    ...d,
                    ...v
                };
                return this.startAxisValueAnimation(y, E);
            });
            return Promise.all(p).then(h);
        }
        startAxisValueAnimation(a, l) {
            const r = this.getAxisMotionValue(a);
            return bf(this.visualElement, a), r.start(id(a, r, 0, l, this.visualElement, !1));
        }
        stopAnimation() {
            ln((a)=>this.getAxisMotionValue(a).stop());
        }
        getAxisMotionValue(a) {
            const l = `_drag${a.toUpperCase()}`, r = this.visualElement.getProps(), u = r[l];
            return u || this.visualElement.getValue(a, (r.initial ? r.initial[a] : void 0) || 0);
        }
        snapToCursor(a) {
            ln((l)=>{
                const { drag: r } = this.getProps();
                if (!wr(l, r, this.currentDirection)) return;
                const { projection: u } = this.visualElement, d = this.getAxisMotionValue(l);
                if (u && u.layout) {
                    const { min: f, max: h } = u.layout.layoutBox[l], m = d.get() || 0;
                    d.set(a[l] - Ut(f, h, .5) + m);
                }
            });
        }
        scalePositionWithinConstraints() {
            if (!this.visualElement.current) return;
            const { drag: a, dragConstraints: l } = this.getProps(), { projection: r } = this.visualElement;
            if (!Si(l) || !r || !this.constraints) return;
            this.stopAnimation();
            const u = {
                x: 0,
                y: 0
            };
            ln((f)=>{
                const h = this.getAxisMotionValue(f);
                if (h && this.constraints !== !1) {
                    const m = h.get();
                    u[f] = vw({
                        min: m,
                        max: m
                    }, this.constraints[f]);
                }
            });
            const { transformTemplate: d } = this.visualElement.getProps();
            this.visualElement.current.style.transform = d ? d({}, "") : "none", r.root && r.root.updateScroll(), r.updateLayout(), this.constraints = !1, this.resolveConstraints(), ln((f)=>{
                if (!wr(f, a, null)) return;
                const h = this.getAxisMotionValue(f), { min: m, max: p } = this.constraints[f];
                h.set(Ut(m, p, u[f]));
            }), this.visualElement.render();
        }
        addListeners() {
            if (!this.visualElement.current) return;
            Sw.set(this.visualElement, this);
            const a = this.visualElement.current, l = Hl(a, "pointerdown", (p)=>{
                const { drag: y, dragListener: v = !0 } = this.getProps(), x = p.target, A = x !== a && s2(x);
                y && v && !A && this.start(p);
            });
            let r;
            const u = ()=>{
                const { dragConstraints: p } = this.getProps();
                Si(p) && p.current && (this.constraints = this.resolveRefConstraints(), r || (r = Ew(a, p.current, ()=>this.scalePositionWithinConstraints())));
            }, { projection: d } = this.visualElement, f = d.addEventListener("measure", u);
            d && !d.layout && (d.root && d.root.updateScroll(), d.updateLayout()), Ot.read(u);
            const h = Ql(window, "resize", ()=>this.scalePositionWithinConstraints()), m = d.addEventListener("didUpdate", (({ delta: p, hasLayoutChanged: y })=>{
                this.isDragging && y && (ln((v)=>{
                    const x = this.getAxisMotionValue(v);
                    x && (this.originPoint[v] += p[v].translate, x.set(x.get() + p[v].translate));
                }), this.visualElement.render());
            }));
            return ()=>{
                h(), l(), f(), m && m(), r && r();
            };
        }
        getProps() {
            const a = this.visualElement.getProps(), { drag: l = !1, dragDirectionLock: r = !1, dragPropagation: u = !1, dragConstraints: d = !1, dragElastic: f = Rf, dragMomentum: h = !0 } = a;
            return {
                ...a,
                drag: l,
                dragDirectionLock: r,
                dragPropagation: u,
                dragConstraints: d,
                dragElastic: f,
                dragMomentum: h
            };
        }
    }
    function n0(n) {
        let a = !0;
        return ()=>{
            if (a) {
                a = !1;
                return;
            }
            n();
        };
    }
    function Ew(n, a, l) {
        const r = oy(n, n0(l)), u = oy(a, n0(l));
        return ()=>{
            r(), u();
        };
    }
    function wr(n, a, l) {
        return (a === !0 || a === n) && (l === null || l === n);
    }
    function Aw(n, a = 10) {
        let l = null;
        return Math.abs(n.y) > a ? l = "y" : Math.abs(n.x) > a && (l = "x"), l;
    }
    class Cw extends ca {
        constructor(a){
            super(a), this.removeGroupControls = Ke, this.removeListeners = Ke, this.controls = new Tw(a);
        }
        mount() {
            const { dragControls: a } = this.node.getProps();
            a && (this.removeGroupControls = a.subscribe(this.controls)), this.removeListeners = this.controls.addListeners() || Ke;
        }
        update() {
            const { dragControls: a } = this.node.getProps(), { dragControls: l } = this.node.prevProps || {};
            a !== l && (this.removeGroupControls(), a && (this.removeGroupControls = a.subscribe(this.controls)));
        }
        unmount() {
            this.removeGroupControls(), this.removeListeners(), this.controls.isDragging || this.controls.endPanSession();
        }
    }
    const Fc = (n)=>(a, l)=>{
            n && Ot.update(()=>n(a, l), !1, !0);
        };
    class ww extends ca {
        constructor(){
            super(...arguments), this.removePointerDownListener = Ke;
        }
        onPointerDown(a) {
            this.session = new Mb(a, this.createPanHandlers(), {
                transformPagePoint: this.node.getTransformPagePoint(),
                contextWindow: Rb(this.node)
            });
        }
        createPanHandlers() {
            const { onPanSessionStart: a, onPanStart: l, onPan: r, onPanEnd: u } = this.node.getProps();
            return {
                onSessionStart: Fc(a),
                onStart: Fc(l),
                onMove: Fc(r),
                onEnd: (d, f)=>{
                    delete this.session, u && Ot.postRender(()=>u(d, f));
                }
            };
        }
        mount() {
            this.removePointerDownListener = Hl(this.node.current, "pointerdown", (a)=>this.onPointerDown(a));
        }
        update() {
            this.session && this.session.updateHandlers(this.createPanHandlers());
        }
        unmount() {
            this.removePointerDownListener(), this.session && this.session.end();
        }
    }
    let $c = !1;
    class _w extends T.Component {
        componentDidMount() {
            const { visualElement: a, layoutGroup: l, switchLayoutGroup: r, layoutId: u } = this.props, { projection: d } = a;
            d && (l.group && l.group.add(d), r && r.register && u && r.register(d), $c && d.root.didUpdate(), d.addEventListener("animationComplete", ()=>{
                this.safeToRemove();
            }), d.setOptions({
                ...d.options,
                layoutDependency: this.props.layoutDependency,
                onExitComplete: ()=>this.safeToRemove()
            })), Ur.hasEverUpdated = !0;
        }
        getSnapshotBeforeUpdate(a) {
            const { layoutDependency: l, visualElement: r, drag: u, isPresent: d } = this.props, { projection: f } = r;
            return f && (f.isPresent = d, a.layoutDependency !== l && f.setOptions({
                ...f.options,
                layoutDependency: l
            }), $c = !0, u || a.layoutDependency !== l || l === void 0 || a.isPresent !== d ? f.willUpdate() : this.safeToRemove(), a.isPresent !== d && (d ? f.promote() : f.relegate() || Ot.postRender(()=>{
                const h = f.getStack();
                (!h || !h.members.length) && this.safeToRemove();
            }))), null;
        }
        componentDidUpdate() {
            const { projection: a } = this.props.visualElement;
            a && (a.root.didUpdate(), cd.postRender(()=>{
                !a.currentAnimation && a.isLead() && this.safeToRemove();
            }));
        }
        componentWillUnmount() {
            const { visualElement: a, layoutGroup: l, switchLayoutGroup: r } = this.props, { projection: u } = a;
            $c = !0, u && (u.scheduleCheckAfterUnmount(), l && l.group && l.group.remove(u), r && r.deregister && r.deregister(u));
        }
        safeToRemove() {
            const { safeToRemove: a } = this.props;
            a && a();
        }
        render() {
            return null;
        }
    }
    function jb(n) {
        const [a, l] = bb(), r = T.useContext(Gf);
        return S.jsx(_w, {
            ...n,
            layoutGroup: r,
            switchLayoutGroup: T.useContext(wb),
            isPresent: a,
            safeToRemove: l
        });
    }
    const Rw = {
        pan: {
            Feature: ww
        },
        drag: {
            Feature: Cw,
            ProjectionNode: vb,
            MeasureLayout: jb
        }
    };
    function a0(n, a, l) {
        const { props: r } = n;
        n.animationState && r.whileHover && n.animationState.setActive("whileHover", l === "Start");
        const u = "onHover" + l, d = r[u];
        d && Ot.postRender(()=>d(a, ns(a)));
    }
    class Mw extends ca {
        mount() {
            const { current: a } = this.node;
            a && (this.unmount = n2(a, (l, r)=>(a0(this.node, r, "Start"), (u)=>a0(this.node, u, "End"))));
        }
        unmount() {}
    }
    class Dw extends ca {
        constructor(){
            super(...arguments), this.isActive = !1;
        }
        onFocus() {
            let a = !1;
            try {
                a = this.node.current.matches(":focus-visible");
            } catch  {
                a = !0;
            }
            !a || !this.node.animationState || (this.node.animationState.setActive("whileFocus", !0), this.isActive = !0);
        }
        onBlur() {
            !this.isActive || !this.node.animationState || (this.node.animationState.setActive("whileFocus", !1), this.isActive = !1);
        }
        mount() {
            this.unmount = Il(Ql(this.node.current, "focus", ()=>this.onFocus()), Ql(this.node.current, "blur", ()=>this.onBlur()));
        }
        unmount() {}
    }
    function i0(n, a, l) {
        const { props: r } = n;
        if (n.current instanceof HTMLButtonElement && n.current.disabled) return;
        n.animationState && r.whileTap && n.animationState.setActive("whileTap", l === "Start");
        const u = "onTap" + (l === "End" ? "" : l), d = r[u];
        d && Ot.postRender(()=>d(a, ns(a)));
    }
    class jw extends ca {
        mount() {
            const { current: a } = this.node;
            if (!a) return;
            const { globalTapTarget: l, propagate: r } = this.node.props;
            this.unmount = o2(a, (u, d)=>(i0(this.node, d, "Start"), (f, { success: h })=>i0(this.node, f, h ? "End" : "Cancel")), {
                useGlobalTarget: l,
                stopPropagation: r?.tap === !1
            });
        }
        unmount() {}
    }
    const Mf = new WeakMap, Jc = new WeakMap, Ow = (n)=>{
        const a = Mf.get(n.target);
        a && a(n);
    }, Nw = (n)=>{
        n.forEach(Ow);
    };
    function zw({ root: n, ...a }) {
        const l = n || document;
        Jc.has(l) || Jc.set(l, {});
        const r = Jc.get(l), u = JSON.stringify(a);
        return r[u] || (r[u] = new IntersectionObserver(Nw, {
            root: n,
            ...a
        })), r[u];
    }
    function Lw(n, a, l) {
        const r = zw(a);
        return Mf.set(n, l), r.observe(n), ()=>{
            Mf.delete(n), r.unobserve(n);
        };
    }
    const Vw = {
        some: 0,
        all: 1
    };
    class Bw extends ca {
        constructor(){
            super(...arguments), this.hasEnteredView = !1, this.isInView = !1;
        }
        startObserver() {
            this.unmount();
            const { viewport: a = {} } = this.node.getProps(), { root: l, margin: r, amount: u = "some", once: d } = a, f = {
                root: l ? l.current : void 0,
                rootMargin: r,
                threshold: typeof u == "number" ? u : Vw[u]
            }, h = (m)=>{
                const { isIntersecting: p } = m;
                if (this.isInView === p || (this.isInView = p, d && !p && this.hasEnteredView)) return;
                p && (this.hasEnteredView = !0), this.node.animationState && this.node.animationState.setActive("whileInView", p);
                const { onViewportEnter: y, onViewportLeave: v } = this.node.getProps(), x = p ? y : v;
                x && x(m);
            };
            return Lw(this.node.current, f, h);
        }
        mount() {
            this.startObserver();
        }
        update() {
            if (typeof IntersectionObserver > "u") return;
            const { props: a, prevProps: l } = this.node;
            [
                "amount",
                "margin",
                "root"
            ].some(Uw(a, l)) && this.startObserver();
        }
        unmount() {}
    }
    function Uw({ viewport: n = {} }, { viewport: a = {} } = {}) {
        return (l)=>n[l] !== a[l];
    }
    const Hw = {
        inView: {
            Feature: Bw
        },
        tap: {
            Feature: jw
        },
        focus: {
            Feature: Dw
        },
        hover: {
            Feature: Mw
        }
    }, Gw = {
        layout: {
            ProjectionNode: vb,
            MeasureLayout: jb
        }
    }, qw = {
        ...cw,
        ...Hw,
        ...Rw,
        ...Gw
    }, ge = lw(qw, sw);
    function Ob(n) {
        const a = Wl(()=>oa(n)), { isStatic: l } = T.useContext(no);
        if (l) {
            const [, r] = T.useState(n);
            T.useEffect(()=>a.on("change", r), []);
        }
        return a;
    }
    function Nb(n, a) {
        const l = Ob(a()), r = ()=>l.set(a());
        return r(), qf(()=>{
            const u = ()=>Ot.preRender(r, !1, !0), d = n.map((f)=>f.on("change", u));
            return ()=>{
                d.forEach((f)=>f()), jn(r);
            };
        }), l;
    }
    function kw(n) {
        Bl.current = [], n();
        const a = Nb(Bl.current, n);
        return Bl.current = void 0, a;
    }
    function Yw(n, a, l, r) {
        if (typeof n == "function") return kw(n);
        const d = typeof a == "function" ? a : y2(a, l, r), f = Array.isArray(n) ? l0(n, d) : l0([
            n
        ], ([m])=>d(m)), h = Array.isArray(n) ? void 0 : n.accelerate;
        return h && !h.isTransformed && typeof a != "function" && Array.isArray(l) && r?.clamp !== !1 && (f.accelerate = {
            ...h,
            times: a,
            keyframes: l,
            isTransformed: !0
        }), f;
    }
    function l0(n, a) {
        const l = Wl(()=>[]);
        return Nb(n, ()=>{
            l.length = 0;
            const r = n.length;
            for(let u = 0; u < r; u++)l[u] = n[u].get();
            return a(l);
        });
    }
    function vd(n) {
        return typeof n == "object" && !Array.isArray(n);
    }
    function zb(n, a, l, r) {
        return n == null ? [] : typeof n == "string" && vd(a) ? ud(n, l, r) : n instanceof NodeList ? Array.from(n) : Array.isArray(n) ? n.filter((u)=>u != null) : [
            n
        ];
    }
    function Xw(n, a, l) {
        return n * (a + 1);
    }
    function s0(n, a, l, r) {
        return typeof a == "number" ? a : a.startsWith("-") || a.startsWith("+") ? Math.max(0, n + parseFloat(a)) : a === "<" ? l : a.startsWith("<") ? Math.max(0, l + parseFloat(a.slice(1))) : r.get(a) ?? n;
    }
    function Kw(n, a, l) {
        for(let r = 0; r < n.length; r++){
            const u = n[r];
            u.at > a && u.at < l && (Ri(n, u), r--);
        }
    }
    function Pw(n, a, l, r, u, d) {
        Kw(n, u, d);
        for(let f = 0; f < a.length; f++)n.push({
            value: a[f],
            at: Ut(u, d, r[f]),
            easing: tv(l, f)
        });
    }
    function Zw(n, a) {
        for(let l = 0; l < n.length; l++)n[l] = n[l] / (a + 1);
    }
    function Qw(n, a) {
        return n.at === a.at ? n.value === null ? 1 : a.value === null ? -1 : 0 : n.at - a.at;
    }
    const Fw = "easeInOut";
    function $w(n, { defaultTransition: a = {}, ...l } = {}, r, u) {
        const d = a.duration || .3, f = new Map, h = new Map, m = {}, p = new Map;
        let y = 0, v = 0, x = 0;
        for(let A = 0; A < n.length; A++){
            const E = n[A];
            if (typeof E == "string") {
                p.set(E, v);
                continue;
            } else if (!Array.isArray(E)) {
                p.set(E.name, s0(v, E.at, y, p));
                continue;
            }
            let [M, R, z = {}] = E;
            z.at !== void 0 && (v = s0(v, z.at, y, p));
            let B = 0;
            const V = (P, U, X, H = 0, Z = 0)=>{
                const Q = Jw(P), { delay: it = 0, times: bt = yv(Q), type: gt = a.type || "keyframes", repeat: Nt, repeatType: ee, repeatDelay: Vt = 0, ...G } = U;
                let { ease: F = a.ease || "easeOut", duration: $ } = U;
                const st = typeof it == "function" ? it(H, Z) : it, ft = Q.length, w = nd(gt) ? gt : u?.[gt || "keyframes"];
                if (ft <= 2 && w) {
                    let lt = 100;
                    if (ft === 2 && t_(Q)) {
                        const Xt = Q[1] - Q[0];
                        lt = Math.abs(Xt);
                    }
                    const ot = {
                        ...a,
                        ...G
                    };
                    $ !== void 0 && (ot.duration = Pe($));
                    const yt = hv(ot, lt, w);
                    F = yt.ease, $ = yt.duration;
                }
                $ ?? ($ = d);
                const Y = v + st;
                bt.length === 1 && bt[0] === 0 && (bt[1] = 1);
                const J = bt.length - Q.length;
                if (J > 0 && gv(bt, J), Q.length === 1 && Q.unshift(null), Nt) {
                    $ = Xw($, Nt);
                    const lt = [
                        ...Q
                    ], ot = [
                        ...bt
                    ];
                    F = Array.isArray(F) ? [
                        ...F
                    ] : [
                        F
                    ];
                    const yt = [
                        ...F
                    ];
                    for(let Xt = 0; Xt < Nt; Xt++){
                        Q.push(...lt);
                        for(let Dt = 0; Dt < lt.length; Dt++)bt.push(ot[Dt] + (Xt + 1)), F.push(Dt === 0 ? "linear" : tv(yt, Dt - 1));
                    }
                    Zw(bt, Nt);
                }
                const W = Y + $;
                Pw(X, Q, F, bt, Y, W), B = Math.max(st + $, B), x = Math.max(W, x);
            };
            if (ie(M)) {
                const P = r0(M, h);
                V(R, z, o0("default", P));
            } else {
                const P = zb(M, R, r, m), U = P.length;
                for(let X = 0; X < U; X++){
                    R = R, z = z;
                    const H = P[X], Z = r0(H, h);
                    for(const Q in R)V(R[Q], Ww(z, Q), o0(Q, Z), X, U);
                }
            }
            y = v, v += B;
        }
        return h.forEach((A, E)=>{
            for(const M in A){
                const R = A[M];
                R.sort(Qw);
                const z = [], B = [], V = [];
                for(let H = 0; H < R.length; H++){
                    const { at: Z, value: Q, easing: it } = R[H];
                    z.push(Q), B.push(Mi(0, x, Z)), V.push(it || "easeOut");
                }
                B[0] !== 0 && (B.unshift(0), z.unshift(z[0]), V.unshift(Fw)), B[B.length - 1] !== 1 && (B.push(1), z.push(null)), f.has(E) || f.set(E, {
                    keyframes: {},
                    transition: {}
                });
                const P = f.get(E);
                P.keyframes[M] = z;
                const { type: U, ...X } = a;
                P.transition[M] = {
                    ...X,
                    duration: x,
                    ease: V,
                    times: B,
                    ...l
                };
            }
        }), f;
    }
    function r0(n, a) {
        return !a.has(n) && a.set(n, {}), a.get(n);
    }
    function o0(n, a) {
        return a[n] || (a[n] = []), a[n];
    }
    function Jw(n) {
        return Array.isArray(n) ? n : [
            n
        ];
    }
    function Ww(n, a) {
        return n && n[a] ? {
            ...n,
            ...n[a]
        } : {
            ...n
        };
    }
    const Iw = (n)=>typeof n == "number", t_ = (n)=>n.every(Iw);
    function e_(n) {
        const a = {
            presenceContext: null,
            props: {},
            visualState: {
                renderState: {
                    transform: {},
                    transformOrigin: {},
                    style: {},
                    vars: {},
                    attrs: {}
                },
                latestValues: {}
            }
        }, l = Ir(n) && !kv(n) ? new lb(a) : new tb(a);
        l.mount(n), Pl.set(n, l);
    }
    function n_(n) {
        const a = {
            presenceContext: null,
            props: {},
            visualState: {
                renderState: {
                    output: {}
                },
                latestValues: {}
            }
        }, l = new z2(a);
        l.mount(n), Pl.set(n, l);
    }
    function a_(n, a) {
        return ie(n) || typeof n == "number" || typeof n == "string" && !vd(a);
    }
    function Lb(n, a, l, r) {
        const u = [];
        if (a_(n, a)) u.push(mb(n, vd(a) && a.default || a, l && (l.default || l)));
        else {
            if (n == null) return u;
            const d = zb(n, a, r), f = d.length;
            for(let h = 0; h < f; h++){
                const m = d[h], p = m instanceof Element ? e_ : n_;
                Pl.has(m) || p(m);
                const y = Pl.get(m), v = {
                    ...l
                };
                "delay" in v && typeof v.delay == "function" && (v.delay = v.delay(h, f)), u.push(...rd(y, {
                    ...a,
                    transition: v
                }, {}));
            }
        }
        return u;
    }
    function i_(n, a, l) {
        const r = [], u = n.map((f)=>{
            if (Array.isArray(f) && typeof f[0] == "function") {
                const h = f[0], m = oa(0);
                return m.on("change", h), f.length === 1 ? [
                    m,
                    [
                        0,
                        1
                    ]
                ] : f.length === 2 ? [
                    m,
                    [
                        0,
                        1
                    ],
                    f[1]
                ] : [
                    m,
                    f[1],
                    f[2]
                ];
            }
            return f;
        });
        return $w(u, a, l, {
            spring: Kl
        }).forEach(({ keyframes: f, transition: h }, m)=>{
            r.push(...Lb(m, f, h));
        }), r;
    }
    function l_(n) {
        return Array.isArray(n) && n.some(Array.isArray);
    }
    function s_(n = {}) {
        const { scope: a, reduceMotion: l } = n;
        function r(u, d, f) {
            let h = [], m;
            if (l_(u)) {
                const { onComplete: y, ...v } = d || {};
                typeof y == "function" && (m = y), h = i_(u, l !== void 0 ? {
                    reduceMotion: l,
                    ...v
                } : v, a);
            } else {
                const { onComplete: y, ...v } = f || {};
                typeof y == "function" && (m = y), h = Lb(u, d, l !== void 0 ? {
                    reduceMotion: l,
                    ...v
                } : v, a);
            }
            const p = new SA(h);
            return m && p.finished.then(m), a && (a.animations.push(p), p.finished.then(()=>{
                Ri(a.animations, p);
            })), p;
        }
        return r;
    }
    const r_ = s_(), u0 = (n)=>{
        let a;
        const l = new Set, r = (p, y)=>{
            const v = typeof p == "function" ? p(a) : p;
            if (!Object.is(v, a)) {
                const x = a;
                a = y ?? (typeof v != "object" || v === null) ? v : Object.assign({}, a, v), l.forEach((A)=>A(a, x));
            }
        }, u = ()=>a, h = {
            setState: r,
            getState: u,
            getInitialState: ()=>m,
            subscribe: (p)=>(l.add(p), ()=>l.delete(p))
        }, m = a = n(r, u, h);
        return h;
    }, o_ = ((n)=>n ? u0(n) : u0), u_ = (n)=>n;
    function c_(n, a = u_) {
        const l = Sr.useSyncExternalStore(n.subscribe, Sr.useCallback(()=>a(n.getState()), [
            n,
            a
        ]), Sr.useCallback(()=>a(n.getInitialState()), [
            n,
            a
        ]));
        return Sr.useDebugValue(l), l;
    }
    const f_ = (n)=>{
        const a = o_(n), l = (r)=>c_(a, r);
        return Object.assign(l, a), l;
    }, bd = ((n)=>f_), d_ = {
        ZoneChanged: 400,
        DamageDealt: 300,
        LifeChanged: 300,
        SpellCast: 500,
        CreatureDestroyed: 400,
        TokenCreated: 400,
        CounterAdded: 200,
        CounterRemoved: 200,
        PermanentTapped: 200,
        PermanentUntapped: 200,
        AttackersDeclared: 300,
        BlockersDeclared: 300
    }, h_ = 200, Nl = bd()((n, a)=>({
            queue: [],
            isPlaying: !1,
            positionRegistry: new Map,
            enqueueEffects: (l)=>{
                const r = l.map((u)=>({
                        type: u.type,
                        data: "data" in u ? u.data : void 0,
                        duration: d_[u.type] ?? h_
                    }));
                n((u)=>({
                        queue: [
                            ...u.queue,
                            ...r
                        ],
                        isPlaying: !0
                    }));
            },
            playNext: ()=>{
                const { queue: l } = a();
                if (l.length === 0) {
                    n({
                        isPlaying: !1
                    });
                    return;
                }
                const [r, ...u] = l;
                return n({
                    queue: u,
                    isPlaying: u.length > 0
                }), r;
            },
            registerPosition: (l, r)=>{
                n((u)=>{
                    const d = new Map(u.positionRegistry);
                    return d.set(l, r), {
                        positionRegistry: d
                    };
                });
            },
            getPosition: (l)=>a().positionRegistry.get(l),
            clearQueue: ()=>n({
                    queue: [],
                    isPlaying: !1
                })
        }));
    function m_({ value: n, position: a, color: l, onComplete: r }) {
        return S.jsx(ge.div, {
            initial: {
                opacity: 1,
                y: 0
            },
            animate: {
                opacity: 0,
                y: -60
            },
            transition: {
                duration: .8
            },
            onAnimationComplete: r,
            style: {
                position: "fixed",
                left: a.x,
                top: a.y,
                pointerEvents: "none",
                color: l,
                fontSize: "1.5rem",
                fontWeight: "bold",
                textShadow: "0 1px 4px rgba(0,0,0,0.8)",
                zIndex: 60
            },
            children: n > 0 ? `+${n}` : n
        });
    }
    const p_ = T.forwardRef(function(a, l) {
        const r = T.useRef(null), u = T.useRef([]), d = T.useRef(0), f = T.useCallback((m, p, y, v)=>{
            for(let x = 0; x < v; x++){
                const A = Math.random() * Math.PI * 2, E = 1 + Math.random() * 3;
                u.current.push({
                    x: m,
                    y: p,
                    vx: Math.cos(A) * E,
                    vy: Math.sin(A) * E,
                    alpha: 1,
                    color: y,
                    decay: .015 + Math.random() * .01
                });
            }
        }, []), h = T.useCallback((m, p, y)=>{
            const v = p.x - m.x, x = p.y - m.y, A = 12;
            for(let E = 0; E < A; E++){
                const M = E / A;
                u.current.push({
                    x: m.x + v * M,
                    y: m.y + x * M,
                    vx: (Math.random() - .5) * .5,
                    vy: (Math.random() - .5) * .5,
                    alpha: 1,
                    color: y,
                    decay: .02 + Math.random() * .01
                });
            }
        }, []);
        return T.useImperativeHandle(l, ()=>({
                emitBurst: f,
                emitTrail: h
            }), [
            f,
            h
        ]), T.useEffect(()=>{
            const m = r.current;
            if (!m) return;
            const p = m.getContext("2d");
            if (!p) return;
            const y = ()=>{
                m.width = window.innerWidth, m.height = window.innerHeight;
            };
            y(), window.addEventListener("resize", y);
            const v = ()=>{
                p.clearRect(0, 0, m.width, m.height);
                const x = [];
                for (const A of u.current)A.x += A.vx, A.y += A.vy, A.alpha -= A.decay, !(A.alpha <= 0) && (p.globalAlpha = A.alpha, p.fillStyle = A.color, p.beginPath(), p.arc(A.x, A.y, 3, 0, Math.PI * 2), p.fill(), x.push(A));
                p.globalAlpha = 1, u.current = x, d.current = requestAnimationFrame(v);
            };
            return d.current = requestAnimationFrame(v), ()=>{
                window.removeEventListener("resize", y), cancelAnimationFrame(d.current);
            };
        }, []), S.jsx("canvas", {
            ref: r,
            style: {
                position: "fixed",
                inset: 0,
                pointerEvents: "none",
                zIndex: 55
            }
        });
    });
    let c0 = 0;
    function g_() {
        const n = Nl((y)=>y.queue), a = Nl((y)=>y.isPlaying), l = Nl((y)=>y.playNext), r = Nl((y)=>y.getPosition), u = T.useRef(null), [d, f] = T.useState([]), h = T.useRef(!1), m = T.useCallback((y)=>{
            const v = y.data;
            switch(y.type){
                case "DamageDealt":
                    {
                        const x = v.target, A = v.amount ?? 0;
                        let E = {
                            x: window.innerWidth / 2,
                            y: window.innerHeight / 2
                        };
                        if (x && "Object" in x) {
                            const R = r(x.Object);
                            R && (E = {
                                x: R.x + R.width / 2,
                                y: R.y
                            });
                        }
                        const M = ++c0;
                        f((R)=>[
                                ...R,
                                {
                                    id: M,
                                    value: -A,
                                    position: E,
                                    color: "#ef4444"
                                }
                            ]);
                        break;
                    }
                case "LifeChanged":
                    {
                        const x = v.amount ?? 0, E = (v.player_id ?? 0) === 0 ? window.innerHeight - 120 : 80, M = window.innerWidth - 140, R = ++c0;
                        f((z)=>[
                                ...z,
                                {
                                    id: R,
                                    value: x,
                                    position: {
                                        x: M,
                                        y: E
                                    },
                                    color: x > 0 ? "#22c55e" : "#ef4444"
                                }
                            ]);
                        break;
                    }
                case "AttackersDeclared":
                    {
                        const x = v.attacker_ids ?? [];
                        for (const A of x){
                            const E = r(A);
                            E && u.current?.emitBurst(E.x + E.width / 2, E.y + E.height / 2, "#ffffff", 8);
                        }
                        break;
                    }
                case "CreatureDestroyed":
                    {
                        const x = v.object_id;
                        if (x != null) {
                            const A = r(x);
                            A && u.current?.emitBurst(A.x + A.width / 2, A.y + A.height / 2, "#ef4444", 16);
                        }
                        break;
                    }
                case "SpellCast":
                    {
                        const x = v.card_id;
                        if (x != null) {
                            const A = r(x);
                            A && u.current?.emitBurst(A.x + A.width / 2, A.y + A.height / 2, "#06b6d4", 12);
                        }
                        break;
                    }
            }
        }, [
            r
        ]);
        T.useEffect(()=>{
            if (!a || n.length === 0 || h.current) return;
            h.current = !0;
            const y = l();
            if (!y) {
                h.current = !1;
                return;
            }
            m(y);
            const v = setTimeout(()=>{
                h.current = !1;
            }, y.duration);
            return ()=>clearTimeout(v);
        }, [
            a,
            n,
            l,
            m
        ]);
        const p = T.useCallback((y)=>{
            f((v)=>v.filter((x)=>x.id !== y));
        }, []);
        return S.jsxs(S.Fragment, {
            children: [
                S.jsx(p_, {
                    ref: u
                }),
                S.jsx(Nn, {
                    children: d.map((y)=>S.jsx(m_, {
                            value: y.value,
                            position: y.position,
                            color: y.color,
                            onComplete: ()=>p(y.id)
                        }, y.id))
                })
            ]
        });
    }
    const y_ = (n)=>(a, l, r)=>{
            const u = r.subscribe;
            return r.subscribe = ((f, h, m)=>{
                let p = f;
                if (h) {
                    const y = m?.equalityFn || Object.is;
                    let v = f(r.getState());
                    p = (x)=>{
                        const A = f(x);
                        if (!y(v, A)) {
                            const E = v;
                            h(v = A, E);
                        }
                    }, m?.fireImmediately && h(v, v);
                }
                return u(p);
            }), n(a, l, r);
        }, v_ = y_, b_ = new Set([
        "PassPriority",
        "DeclareAttackers",
        "DeclareBlockers",
        "ActivateAbility"
    ]), f0 = {
        gameState: null,
        events: [],
        adapter: null,
        waitingFor: null,
        stateHistory: []
    }, Tt = bd()(v_((n, a)=>({
            ...f0,
            initGame: async (l, r)=>{
                await l.initialize();
                const u = await l.getState();
                n({
                    adapter: l,
                    gameState: u,
                    waitingFor: u.waiting_for,
                    events: [],
                    stateHistory: []
                });
            },
            dispatch: async (l)=>{
                const { adapter: r, gameState: u } = a();
                if (!r || !u) throw new Error("Game not initialized");
                const d = b_.has(l.type), f = await r.submitAction(l), h = await r.getState();
                return n((m)=>{
                    const p = d ? [
                        ...m.stateHistory,
                        u
                    ].slice(-5) : m.stateHistory;
                    return {
                        gameState: h,
                        events: f,
                        waitingFor: h.waiting_for,
                        stateHistory: p
                    };
                }), f;
            },
            undo: ()=>{
                const { stateHistory: l } = a();
                if (l.length === 0) return;
                const r = l[l.length - 1];
                n({
                    gameState: r,
                    waitingFor: r.waiting_for,
                    events: [],
                    stateHistory: l.slice(0, -1)
                });
            },
            reset: ()=>{
                const { adapter: l } = a();
                l && l.dispose(), n(f0);
            }
        })));
    function xd(n) {
        return new Promise((a, l)=>{
            n.oncomplete = n.onsuccess = ()=>a(n.result), n.onabort = n.onerror = ()=>l(n.error);
        });
    }
    function x_(n, a) {
        let l;
        const r = ()=>{
            if (l) return l;
            const u = indexedDB.open(n);
            return u.onupgradeneeded = ()=>u.result.createObjectStore(a), l = xd(u), l.then((d)=>{
                d.onclose = ()=>l = void 0;
            }, ()=>{}), l;
        };
        return (u, d)=>r().then((f)=>d(f.transaction(a, u).objectStore(a)));
    }
    let Wc;
    function Vb() {
        return Wc || (Wc = x_("keyval-store", "keyval")), Wc;
    }
    function Bb(n, a = Vb()) {
        return a("readonly", (l)=>xd(l.get(n)));
    }
    function S_(n, a, l = Vb()) {
        return l("readwrite", (r)=>(r.put(a, n), xd(r.transaction)));
    }
    function Ub(n, a) {
        return `scryfall:${n}:${a}`;
    }
    async function T_(n, a) {
        const l = await Bb(Ub(n, a));
        return l ? URL.createObjectURL(l) : null;
    }
    async function E_(n, a, l) {
        await S_(Ub(n, a), l);
    }
    function d0(n) {
        URL.revokeObjectURL(n);
    }
    const h0 = 75;
    let m0 = 0;
    async function Sd(n) {
        const l = Date.now() - m0;
        return l < h0 && await new Promise((r)=>setTimeout(r, h0 - l)), m0 = Date.now(), fetch(n);
    }
    async function A_(n) {
        const a = `https://api.scryfall.com/cards/named?exact=${encodeURIComponent(n)}`, l = await Sd(a);
        if (!l.ok) throw new Error(`Scryfall API error: ${l.status} for "${n}"`);
        return l.json();
    }
    function C_(n, a, l) {
        if (n.card_faces?.[l]?.image_uris?.[a]) return n.card_faces[l].image_uris[a];
        if (n.image_uris?.[a]) return n.image_uris[a];
        throw new Error("No image URI found for card");
    }
    async function w_(n, a = "normal") {
        const l = await Bb(`scryfall:${n}:${a}`);
        if (l) return l;
        const r = await A_(n), u = C_(r, a, 0), f = await (await Sd(u)).blob();
        return await E_(n, a, f), f;
    }
    async function Hb(n, a) {
        const l = `https://api.scryfall.com/cards/search?q=${encodeURIComponent(n)}`, r = await Sd(l);
        if (a?.aborted) return {
            cards: [],
            total: 0
        };
        if (r.status === 429) {
            const d = parseInt(r.headers.get("Retry-After") ?? "1", 10);
            return await new Promise((f)=>setTimeout(f, d * 1e3)), Hb(n, a);
        }
        if (r.status === 404) return {
            cards: [],
            total: 0
        };
        if (!r.ok) throw new Error(`Scryfall search error: ${r.status}`);
        const u = await r.json();
        return {
            cards: u.data,
            total: u.total_cards
        };
    }
    function __(n) {
        const a = [];
        return n.text && a.push(n.text), n.colors?.length && a.push(`c:${n.colors.join("")}`), n.type && a.push(`t:${n.type}`), n.cmcMin !== void 0 && a.push(`cmc>=${n.cmcMin}`), n.cmcMax !== void 0 && a.push(`cmc<=${n.cmcMax}`), n.format && a.push(`f:${n.format}`), a.join(" ");
    }
    function R_(n) {
        return n.image_uris?.small ?? n.card_faces?.[0]?.image_uris?.small ?? "";
    }
    function Gb(n, a) {
        const l = a?.size ?? "normal", [r, u] = T.useState(null), [d, f] = T.useState(!0);
        return T.useEffect(()=>{
            let h = !1, m = null;
            async function p() {
                f(!0), u(null);
                try {
                    const y = await T_(n, l);
                    if (y) {
                        h ? d0(y) : (m = y, u(y), f(!1));
                        return;
                    }
                    const v = await w_(n, l);
                    if (!h) {
                        const x = URL.createObjectURL(v);
                        m = x, u(x), f(!1);
                    }
                } catch  {
                    h || f(!1);
                }
            }
            return p(), ()=>{
                h = !0, m && d0(m);
            };
        }, [
            n,
            l
        ]), {
            src: r,
            isLoading: d
        };
    }
    function Td({ cardName: n, size: a = "normal", faceIndex: l, className: r = "", tapped: u = !1 }) {
        const { src: d, isLoading: f } = Gb(n, {
            size: a
        }), m = `w-[var(--card-w)] h-[var(--card-h)] rounded-lg transition-transform duration-200 ${u ? "rotate-[30deg]" : ""} ${r}`;
        return f || !d ? S.jsx("div", {
            className: `${m} bg-gray-700 border border-gray-600 shadow-md animate-pulse`,
            "aria-label": `Loading ${n}`
        }) : S.jsx("img", {
            src: d,
            alt: n,
            className: `${m} border border-gray-600 shadow-md object-cover`,
            draggable: !1
        });
    }
    function M_(n, a) {
        const { delay: l = 500 } = {}, r = T.useRef(null), u = T.useCallback(()=>{
            r.current && (clearTimeout(r.current), r.current = null);
        }, []), d = T.useCallback(()=>{
            r.current = setTimeout(n, l);
        }, [
            n,
            l
        ]), f = T.useCallback(()=>{
            u();
        }, [
            u
        ]), h = T.useCallback(()=>{
            u();
        }, [
            u
        ]);
        return {
            onTouchStart: d,
            onTouchEnd: f,
            onTouchCancel: h
        };
    }
    const It = bd()((n)=>({
            selectedObjectId: null,
            hoveredObjectId: null,
            inspectedObjectId: null,
            targetingMode: !1,
            validTargetIds: [],
            sourceObjectId: null,
            selectedTargets: [],
            fullControl: !1,
            autoPass: !1,
            selectObject: (a)=>n({
                    selectedObjectId: a
                }),
            hoverObject: (a)=>n({
                    hoveredObjectId: a
                }),
            inspectObject: (a)=>n({
                    inspectedObjectId: a
                }),
            startTargeting: (a, l)=>n({
                    targetingMode: !0,
                    validTargetIds: a,
                    sourceObjectId: l,
                    selectedTargets: []
                }),
            addTarget: (a)=>n((l)=>({
                        selectedTargets: [
                            ...l.selectedTargets,
                            a
                        ]
                    })),
            clearTargets: ()=>n({
                    targetingMode: !1,
                    validTargetIds: [],
                    sourceObjectId: null,
                    selectedTargets: []
                }),
            toggleFullControl: ()=>n((a)=>({
                        fullControl: !a.fullControl
                    })),
            toggleAutoPass: ()=>n((a)=>({
                        autoPass: !a.autoPass
                    }))
        })), D_ = {
        Plus1Plus1: "bg-green-600",
        Minus1Minus1: "bg-red-600",
        Loyalty: "bg-amber-600"
    };
    function qb({ objectId: n }) {
        const a = Tt((X)=>X.gameState?.objects[n]), l = Tt((X)=>X.gameState?.turn_number ?? 0), r = It((X)=>X.selectedObjectId), u = It((X)=>X.targetingMode), d = It((X)=>X.selectedTargets), f = It((X)=>X.selectObject), h = It((X)=>X.addTarget), m = It((X)=>X.hoverObject), p = It((X)=>X.inspectObject), y = M_(T.useCallback(()=>{
            p(n);
        }, [
            p,
            n
        ]));
        if (!a) return null;
        const x = a.card_types.core_types.includes("Creature") && a.entered_battlefield_turn === l && !a.keywords.some((X)=>X.toLowerCase() === "haste"), A = It((X)=>X.validTargetIds), E = r === n, M = d.includes(n), R = u && A.includes(n);
        let z = "";
        M ? z = "ring-2 ring-cyan-400 shadow-[0_0_10px_2px_rgba(34,211,238,0.5)]" : R ? z = "ring-2 ring-cyan-400/60 shadow-[0_0_12px_3px_rgba(0,229,255,0.8)]" : E && (z = "ring-2 ring-white shadow-[0_0_8px_2px_rgba(255,255,255,0.6)]");
        const B = x ? "saturate(50%)" : void 0, V = x ? "0 0 6px 1px rgba(255,255,255,0.3)" : void 0, P = Object.entries(a.counters), U = ()=>{
            u ? h(n) : f(E ? null : n);
        };
        return S.jsxs(ge.div, {
            "data-object-id": n,
            layoutId: `permanent-${n}`,
            className: `relative cursor-pointer rounded-lg ${z}`,
            style: {
                filter: B,
                boxShadow: V
            },
            onClick: U,
            onMouseEnter: ()=>m(n),
            onMouseLeave: ()=>m(null),
            ...y,
            children: [
                a.attachments.map((X, H)=>S.jsx("div", {
                        className: "absolute left-0 top-0 z-0",
                        style: {
                            transform: `translateY(${-(H + 1) * 10}px)`
                        },
                        children: S.jsx(qb, {
                            objectId: X
                        })
                    }, X)),
                S.jsx("div", {
                    className: "relative z-10",
                    children: S.jsx(Td, {
                        cardName: a.name,
                        tapped: a.tapped,
                        size: "small"
                    })
                }),
                a.damage_marked > 0 && S.jsxs("div", {
                    className: "absolute inset-x-0 bottom-0 z-20 flex h-6 items-center justify-center rounded-b-lg bg-red-600/60 text-xs font-bold text-white",
                    children: [
                        "-",
                        a.damage_marked
                    ]
                }),
                P.length > 0 && S.jsx("div", {
                    className: "absolute bottom-1 right-1 z-20 flex flex-col gap-0.5",
                    children: P.map(([X, H])=>S.jsxs("span", {
                            className: `rounded px-1 text-[10px] font-bold text-white ${D_[X] ?? "bg-purple-600"}`,
                            children: [
                                j_(X),
                                " x",
                                H
                            ]
                        }, X))
                })
            ]
        });
    }
    function j_(n) {
        return n === "Plus1Plus1" ? "+1/+1" : n === "Minus1Minus1" ? "-1/-1" : n;
    }
    const O_ = {
        creatures: "Creatures",
        lands: "Lands",
        other: "Other"
    };
    function xi({ objectIds: n, rowType: a }) {
        return n.length === 0 ? null : S.jsxs("div", {
            className: "flex min-h-[calc(var(--card-h)+8px)] flex-wrap items-center gap-2 px-2",
            children: [
                S.jsx("span", {
                    className: "text-[10px] font-medium uppercase tracking-wider text-gray-600 [writing-mode:vertical-lr]",
                    children: O_[a]
                }),
                n.map((l)=>S.jsx(qb, {
                        objectId: l
                    }, l))
            ]
        });
    }
    function p0(n) {
        const a = [], l = [], r = [];
        for (const u of n)u.card_types.core_types.includes("Land") ? l.push(u.id) : u.card_types.core_types.includes("Creature") ? a.push(u.id) : r.push(u.id);
        return {
            creatures: a,
            lands: l,
            other: r
        };
    }
    function N_() {
        const n = Tt((r)=>r.gameState), { opponent: a, player: l } = T.useMemo(()=>{
            if (!n) return {
                opponent: null,
                player: null
            };
            const r = n.battlefield.map((f)=>n.objects[f]).filter(Boolean), u = r.filter((f)=>f.controller === 0), d = r.filter((f)=>f.controller === 1);
            return {
                player: p0(u),
                opponent: p0(d)
            };
        }, [
            n
        ]);
        return n ? S.jsxs("div", {
            className: "flex flex-1 flex-col bg-gray-950",
            children: [
                S.jsx("div", {
                    className: "flex flex-col gap-1 border-b border-gray-800 py-1",
                    children: a && S.jsxs(S.Fragment, {
                        children: [
                            S.jsx(xi, {
                                objectIds: a.other,
                                rowType: "other"
                            }),
                            S.jsx(xi, {
                                objectIds: a.creatures,
                                rowType: "creatures"
                            }),
                            S.jsx(xi, {
                                objectIds: a.lands,
                                rowType: "lands"
                            })
                        ]
                    })
                }),
                S.jsx("div", {
                    className: "flex min-h-[40px] flex-1 items-center justify-center",
                    children: S.jsxs("span", {
                        className: "text-xs text-gray-600",
                        children: [
                            "Turn ",
                            n.turn_number,
                            " · ",
                            n.phase
                        ]
                    })
                }),
                S.jsx("div", {
                    className: "flex flex-col gap-1 border-t border-gray-800 py-1",
                    children: l && S.jsxs(S.Fragment, {
                        children: [
                            S.jsx(xi, {
                                objectIds: l.other,
                                rowType: "other"
                            }),
                            S.jsx(xi, {
                                objectIds: l.creatures,
                                rowType: "creatures"
                            }),
                            S.jsx(xi, {
                                objectIds: l.lands,
                                rowType: "lands"
                            })
                        ]
                    })
                })
            ]
        }) : S.jsx("div", {
            className: "flex flex-1 items-center justify-center",
            children: S.jsx("span", {
                className: "text-gray-500",
                children: "Waiting for game..."
            })
        });
    }
    function z_({ cardName: n, faceIndex: a, position: l }) {
        return S.jsx(Nn, {
            children: n && S.jsx(L_, {
                cardName: n,
                faceIndex: a,
                position: l
            })
        });
    }
    function L_({ cardName: n, faceIndex: a, position: l }) {
        const { src: r, isLoading: u } = Gb(n, {
            size: "large"
        }), d = l ? {
            left: Math.min(l.x + 16, window.innerWidth - 488),
            top: Math.min(l.y - 200, window.innerHeight - 736)
        } : {
            right: 16,
            top: 16
        };
        return S.jsx(ge.div, {
            className: "fixed z-50 pointer-events-none",
            style: d,
            initial: {
                opacity: 0,
                scale: .9
            },
            animate: {
                opacity: 1,
                scale: 1
            },
            exit: {
                opacity: 0,
                scale: .9
            },
            transition: {
                duration: .15
            },
            children: u || !r ? S.jsx("div", {
                className: "w-[472px] h-[659px] rounded-xl bg-gray-700 border border-gray-600 shadow-2xl animate-pulse"
            }) : S.jsx("img", {
                src: r,
                alt: n,
                className: "w-[472px] h-[659px] rounded-xl border border-gray-600 shadow-2xl object-cover",
                draggable: !1
            })
        });
    }
    function V_() {
        const n = It((l)=>l.fullControl), a = It((l)=>l.toggleFullControl);
        return S.jsxs("button", {
            onClick: a,
            className: `rounded px-3 py-1 text-xs font-semibold transition-colors ${n ? "bg-amber-600 text-white" : "bg-gray-700 text-gray-400 hover:bg-gray-600"}`,
            children: [
                "Full Control: ",
                n ? "ON" : "OFF"
            ]
        });
    }
    function g0({ playerId: n }) {
        const a = Tt((f)=>f.gameState?.players[n]?.life ?? 20), l = T.useRef(a), r = Ob(a), u = Yw(r, (f)=>Math.round(f));
        T.useEffect(()=>{
            l.current !== a && (r_(r, a, {
                duration: .3
            }), l.current = a);
        }, [
            a,
            r
        ]);
        const d = a >= 10 ? "text-green-400" : a >= 5 ? "text-yellow-400" : "text-red-400";
        return S.jsxs("div", {
            className: "flex items-center gap-2",
            children: [
                S.jsxs("span", {
                    className: "text-xs text-gray-400",
                    children: [
                        "P",
                        n + 1
                    ]
                }),
                S.jsx(ge.span, {
                    initial: {
                        scale: 1.3
                    },
                    animate: {
                        scale: 1
                    },
                    transition: {
                        duration: .2
                    },
                    className: `text-xl font-bold tabular-nums ${d}`,
                    children: S.jsx(ge.span, {
                        children: u
                    })
                }, a)
            ]
        });
    }
    function B_() {
        const n = Tt((r)=>r.waitingFor), a = Tt((r)=>r.dispatch);
        return n?.type === "Priority" ? S.jsx("button", {
            onClick: ()=>a({
                    type: "PassPriority"
                }),
            className: "rounded-lg bg-blue-600 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-blue-500 active:bg-blue-700",
            children: "Pass Priority"
        }) : null;
    }
    const U_ = [
        {
            key: "Untap",
            label: "UNT"
        },
        {
            key: "Upkeep",
            label: "UPK"
        },
        {
            key: "Draw",
            label: "DRW"
        },
        {
            key: "PreCombatMain",
            label: "M1"
        },
        {
            key: "BeginCombat",
            label: "BC"
        },
        {
            key: "DeclareAttackers",
            label: "ATK"
        },
        {
            key: "DeclareBlockers",
            label: "BLK"
        },
        {
            key: "CombatDamage",
            label: "DMG"
        },
        {
            key: "EndCombat",
            label: "EC"
        },
        {
            key: "PostCombatMain",
            label: "M2"
        },
        {
            key: "End",
            label: "END"
        },
        {
            key: "Cleanup",
            label: "CLN"
        }
    ];
    function H_() {
        const n = Tt((l)=>l.gameState?.phase ?? "Untap"), a = Tt((l)=>l.gameState?.turn_number ?? 0);
        return S.jsxs("div", {
            className: "flex flex-col gap-1",
            children: [
                S.jsxs("div", {
                    className: "text-center text-xs font-semibold text-gray-300",
                    children: [
                        "Turn ",
                        a
                    ]
                }),
                S.jsx("div", {
                    className: "flex flex-wrap gap-0.5",
                    children: U_.map(({ key: l, label: r })=>{
                        const u = l === n;
                        return S.jsx("span", {
                            className: `rounded px-1 py-0.5 text-[10px] font-bold transition-colors ${u ? "bg-white/20 text-white shadow-[0_0_6px_1px_rgba(255,255,255,0.4)]" : "text-gray-600"}`,
                            children: r
                        }, l);
                    })
                })
            ]
        });
    }
    function G_() {
        const n = Tt((l)=>l.gameState?.players[1]);
        if (!n) return null;
        const a = n.hand.length;
        return a === 0 ? null : S.jsxs("div", {
            className: "flex items-center justify-center gap-1 border-b border-gray-800 bg-gray-900/80 px-4 py-2",
            children: [
                Array.from({
                    length: a
                }, (l, r)=>S.jsx("div", {
                        className: "h-[var(--card-h)] w-[var(--card-w)] rounded-lg border border-gray-600 bg-gradient-to-br from-gray-800 via-gray-700 to-gray-800 shadow-md",
                        style: {
                            marginLeft: r > 0 ? "-12px" : void 0
                        },
                        children: S.jsx("div", {
                            className: "flex h-full items-center justify-center",
                            children: S.jsx("div", {
                                className: "h-[70%] w-[70%] rounded border border-gray-500 bg-gradient-to-br from-amber-900/40 via-amber-800/30 to-amber-900/40"
                            })
                        })
                    }, r)),
                a > 5 && S.jsx("span", {
                    className: "ml-2 rounded bg-gray-700 px-1.5 py-0.5 text-xs font-medium text-gray-300",
                    children: a
                })
            ]
        });
    }
    function q_() {
        const n = Tt((m)=>m.gameState?.players[0]), a = Tt((m)=>m.gameState?.objects), l = Tt((m)=>m.waitingFor), r = Tt((m)=>m.dispatch), u = It((m)=>m.inspectObject);
        if (!n || !a) return null;
        const d = n.hand.map((m)=>a[m]).filter(Boolean), f = l?.type === "Priority" && l.data.player === 0, h = (m, p, y)=>{
            f && (y.includes("Land") ? r({
                type: "PlayLand",
                data: {
                    card_id: a[m].card_id
                }
            }) : r({
                type: "CastSpell",
                data: {
                    card_id: a[m].card_id,
                    targets: []
                }
            }));
        };
        return S.jsx("div", {
            className: "flex items-end justify-center gap-[-8px] border-t border-gray-800 bg-gray-900/80 px-4 py-2",
            children: S.jsx(Nn, {
                children: d.map((m)=>S.jsx(ge.div, {
                        layout: !0,
                        initial: {
                            opacity: 0,
                            y: 30
                        },
                        animate: {
                            opacity: 1,
                            y: 0
                        },
                        exit: {
                            opacity: 0,
                            y: 30
                        },
                        whileHover: {
                            y: -20
                        },
                        transition: {
                            duration: .2
                        },
                        className: `relative cursor-pointer ${f ? "shadow-[0_0_8px_2px_rgba(255,255,255,0.6)]" : ""} rounded-lg`,
                        style: {
                            marginLeft: "-8px",
                            marginRight: "-8px"
                        },
                        onClick: ()=>h(m.id, m.name, m.card_types.core_types),
                        onMouseEnter: ()=>u(m.id),
                        onMouseLeave: ()=>u(null),
                        children: S.jsx(Td, {
                            cardName: m.name,
                            size: "small"
                        })
                    }, m.id))
            })
        });
    }
    function k_(n) {
        switch(n.type){
            case "GameStarted":
                return "Game started";
            case "TurnStarted":
                return `Turn ${n.data.turn_number} -- Player ${n.data.player_id + 1}`;
            case "PhaseChanged":
                return `Phase: ${n.data.phase}`;
            case "PriorityPassed":
                return `Player ${n.data.player_id + 1} passed priority`;
            case "SpellCast":
                return `Spell cast by Player ${n.data.controller + 1}`;
            case "AbilityActivated":
                return `Ability activated (source ${n.data.source_id})`;
            case "ZoneChanged":
                return `Object ${n.data.object_id} moved ${n.data.from} -> ${n.data.to}`;
            case "LifeChanged":
                {
                    const a = n.data.amount >= 0 ? "+" : "";
                    return `Player ${n.data.player_id + 1} life: ${a}${n.data.amount}`;
                }
            case "ManaAdded":
                return `Player ${n.data.player_id + 1} added ${n.data.mana_type} mana`;
            case "PermanentTapped":
                return `Permanent ${n.data.object_id} tapped`;
            case "PermanentUntapped":
                return `Permanent ${n.data.object_id} untapped`;
            case "PlayerLost":
                return `Player ${n.data.player_id + 1} lost the game`;
            case "MulliganStarted":
                return "Mulligan phase";
            case "CardsDrawn":
                return `Player ${n.data.player_id + 1} drew ${n.data.count} card(s)`;
            case "CardDrawn":
                return `Player ${n.data.player_id + 1} drew a card`;
            case "LandPlayed":
                return `Player ${n.data.player_id + 1} played a land`;
            case "StackPushed":
                return `Object ${n.data.object_id} pushed to stack`;
            case "StackResolved":
                return `Stack entry ${n.data.object_id} resolved`;
            case "Discarded":
                return `Player ${n.data.player_id + 1} discarded`;
            case "DamageCleared":
                return `Damage cleared from ${n.data.object_id}`;
            case "GameOver":
                return n.data.winner != null ? `Game over -- Player ${n.data.winner + 1} wins!` : "Game over -- Draw";
            case "DamageDealt":
                {
                    const a = "Player" in n.data.target ? `Player ${n.data.target.Player + 1}` : `object ${n.data.target.Object}`;
                    return `Source ${n.data.source_id} deals ${n.data.amount} damage to ${a}`;
                }
            case "SpellCountered":
                return `Object ${n.data.object_id} countered by ${n.data.countered_by}`;
            case "CounterAdded":
                return `${n.data.counter_type} x${n.data.count} added to ${n.data.object_id}`;
            case "CounterRemoved":
                return `${n.data.counter_type} x${n.data.count} removed from ${n.data.object_id}`;
            case "TokenCreated":
                return `Token "${n.data.name}" created`;
            case "CreatureDestroyed":
                return `Creature ${n.data.object_id} destroyed`;
            case "PermanentSacrificed":
                return `Player ${n.data.player_id + 1} sacrificed ${n.data.object_id}`;
            case "EffectResolved":
                return `Effect ${n.data.api_type} resolved`;
            case "AttackersDeclared":
                return `${n.data.attacker_ids.length} attacker(s) declared`;
            case "BlockersDeclared":
                return `${n.data.assignments.length} blocker(s) assigned`;
            case "BecomesTarget":
                return `Object ${n.data.object_id} targeted by ${n.data.source_id}`;
            case "ReplacementApplied":
                return `Replacement applied: ${n.data.event_type}`;
            default:
                return `Event: ${n.type}`;
        }
    }
    function Y_() {
        const n = Tt((l)=>l.events), a = T.useRef(null);
        return T.useEffect(()=>{
            const l = a.current;
            l && (l.scrollTop = l.scrollHeight);
        }, [
            n
        ]), S.jsxs("div", {
            className: "flex flex-1 flex-col gap-1 overflow-hidden",
            children: [
                S.jsx("h3", {
                    className: "text-xs font-semibold uppercase tracking-wider text-gray-400",
                    children: "Game Log"
                }),
                S.jsx("div", {
                    ref: a,
                    className: "flex-1 overflow-y-auto rounded bg-gray-900 p-1.5 font-mono text-[10px] leading-relaxed text-gray-300",
                    children: n.length === 0 ? S.jsx("p", {
                        className: "italic text-gray-600",
                        children: "No events yet"
                    }) : n.map((l, r)=>S.jsx("div", {
                            className: "border-b border-gray-800 py-0.5",
                            children: k_(l)
                        }, r))
                })
            ]
        });
    }
    const X_ = {
        White: "bg-yellow-400 text-black",
        Blue: "bg-blue-500 text-white",
        Black: "bg-gray-800 text-white",
        Red: "bg-red-500 text-white",
        Green: "bg-green-600 text-white",
        Colorless: "bg-gray-500 text-white"
    };
    function K_({ color: n, amount: a }) {
        return a <= 0 ? null : S.jsx("span", {
            className: `inline-flex h-7 w-7 items-center justify-center rounded-full text-xs font-bold ${X_[n]}`,
            children: a
        });
    }
    const P_ = [
        "White",
        "Blue",
        "Black",
        "Red",
        "Green",
        "Colorless"
    ];
    function Z_() {
        const n = Tt((y)=>y.waitingFor), a = Tt((y)=>y.gameState), l = Tt((y)=>y.dispatch), r = n?.type === "ManaPayment", u = r ? n.data.player : null, d = u != null ? a?.players[u] : null, f = T.useMemo(()=>{
            if (!d) return [];
            const y = {
                White: 0,
                Blue: 0,
                Black: 0,
                Red: 0,
                Green: 0,
                Colorless: 0
            };
            for (const v of d.mana_pool.mana)y[v.color]++;
            return P_.filter((v)=>y[v] > 0).map((v)=>({
                    color: v,
                    amount: y[v]
                }));
        }, [
            d
        ]), h = T.useMemo(()=>!a || u == null ? [] : a.battlefield.map((y)=>a.objects[y]).filter((y)=>y && y.controller === u && y.card_types.core_types.includes("Land") && !y.tapped), [
            a,
            u
        ]), m = T.useCallback((y)=>{
            l({
                type: "TapLandForMana",
                data: {
                    object_id: y
                }
            });
        }, [
            l
        ]), p = T.useCallback(()=>{
            l({
                type: "PassPriority"
            });
        }, [
            l
        ]);
        return !r || !d ? null : S.jsx(Nn, {
            children: S.jsx(ge.div, {
                className: "fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4",
                initial: {
                    y: 80,
                    opacity: 0
                },
                animate: {
                    y: 0,
                    opacity: 1
                },
                exit: {
                    y: 80,
                    opacity: 0
                },
                transition: {
                    duration: .25
                },
                children: S.jsxs("div", {
                    className: "rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700",
                    children: [
                        S.jsx("h3", {
                            className: "mb-3 text-center text-sm font-semibold text-gray-300",
                            children: "Pay Mana Cost"
                        }),
                        S.jsxs("div", {
                            className: "mb-3 flex items-center justify-center gap-2",
                            children: [
                                S.jsx("span", {
                                    className: "text-xs text-gray-500",
                                    children: "Pool:"
                                }),
                                f.length > 0 ? f.map(({ color: y, amount: v })=>S.jsx(K_, {
                                        color: y,
                                        amount: v
                                    }, y)) : S.jsx("span", {
                                    className: "text-xs text-gray-600",
                                    children: "Empty"
                                })
                            ]
                        }),
                        h.length > 0 && S.jsxs("div", {
                            className: "mb-3",
                            children: [
                                S.jsx("p", {
                                    className: "mb-1 text-center text-xs text-gray-500",
                                    children: "Tap a land for mana:"
                                }),
                                S.jsx("div", {
                                    className: "flex flex-wrap justify-center gap-1",
                                    children: h.map((y)=>S.jsx("button", {
                                            onClick: ()=>m(y.id),
                                            className: "rounded bg-gray-800 px-2 py-1 text-xs text-white ring-1 ring-white/20 transition hover:bg-gray-700 hover:ring-white/50",
                                            children: y.name
                                        }, y.id))
                                })
                            ]
                        }),
                        S.jsx("div", {
                            className: "flex justify-center gap-3",
                            children: S.jsx("button", {
                                onClick: p,
                                className: "rounded-lg bg-cyan-600 px-5 py-1.5 text-sm font-semibold text-white transition hover:bg-cyan-500",
                                children: "Auto Pay"
                            })
                        })
                    ]
                })
            })
        });
    }
    function Q_({ title: n, options: a, onChoose: l }) {
        return S.jsx(Nn, {
            children: S.jsxs(ge.div, {
                className: "fixed inset-0 z-50 flex items-center justify-center",
                initial: {
                    opacity: 0
                },
                animate: {
                    opacity: 1
                },
                exit: {
                    opacity: 0
                },
                transition: {
                    duration: .2
                },
                children: [
                    S.jsx("div", {
                        className: "absolute inset-0 bg-black/60"
                    }),
                    S.jsxs(ge.div, {
                        className: "relative z-10 w-full max-w-sm rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700",
                        initial: {
                            scale: .9,
                            opacity: 0
                        },
                        animate: {
                            scale: 1,
                            opacity: 1
                        },
                        exit: {
                            scale: .9,
                            opacity: 0
                        },
                        transition: {
                            duration: .2,
                            ease: "easeOut"
                        },
                        children: [
                            S.jsx("h2", {
                                className: "mb-4 text-center text-lg font-bold text-white",
                                children: n
                            }),
                            S.jsx("div", {
                                className: "flex flex-col gap-2",
                                children: a.map((r)=>S.jsxs("button", {
                                        onClick: ()=>l(r.id),
                                        className: "rounded-lg bg-gray-800 px-4 py-3 text-left transition hover:bg-gray-700 hover:ring-1 hover:ring-cyan-400/50",
                                        children: [
                                            S.jsx("span", {
                                                className: "font-semibold text-white",
                                                children: r.label
                                            }),
                                            r.description && S.jsx("p", {
                                                className: "mt-1 text-xs text-gray-400",
                                                children: r.description
                                            })
                                        ]
                                    }, r.id))
                            })
                        ]
                    })
                ]
            })
        });
    }
    function F_() {
        const n = Tt((f)=>f.waitingFor), a = Tt((f)=>f.dispatch), l = n?.type === "ReplacementChoice", r = l ? n.data.candidate_count : 0, u = T.useCallback((f)=>{
            a({
                type: "ChooseReplacement",
                data: {
                    index: f
                }
            });
        }, [
            a
        ]);
        if (!l || r === 0) return null;
        const d = Array.from({
            length: r
        }, (f, h)=>h);
        return S.jsx(Nn, {
            children: S.jsxs(ge.div, {
                className: "fixed inset-0 z-50 flex items-center justify-center",
                initial: {
                    opacity: 0
                },
                animate: {
                    opacity: 1
                },
                exit: {
                    opacity: 0
                },
                transition: {
                    duration: .2
                },
                children: [
                    S.jsx("div", {
                        className: "absolute inset-0 bg-black/60"
                    }),
                    S.jsxs(ge.div, {
                        className: "relative z-10 w-full max-w-md rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700",
                        initial: {
                            scale: .9,
                            opacity: 0
                        },
                        animate: {
                            scale: 1,
                            opacity: 1
                        },
                        exit: {
                            scale: .9,
                            opacity: 0
                        },
                        transition: {
                            duration: .2,
                            ease: "easeOut"
                        },
                        children: [
                            S.jsx("h2", {
                                className: "mb-2 text-center text-lg font-bold text-white",
                                children: "Replacement Effects"
                            }),
                            S.jsx("p", {
                                className: "mb-4 text-center text-sm text-gray-400",
                                children: "Choose which replacement effect applies first"
                            }),
                            S.jsx("div", {
                                className: "flex flex-col gap-2",
                                children: d.map((f)=>S.jsx("button", {
                                        onClick: ()=>u(f),
                                        className: "rounded-lg bg-gray-800 px-4 py-3 text-left transition hover:bg-gray-700 hover:ring-1 hover:ring-cyan-400/50",
                                        children: S.jsxs("span", {
                                            className: "font-semibold text-white",
                                            children: [
                                                "Replacement Effect ",
                                                f + 1
                                            ]
                                        })
                                    }, f))
                            })
                        ]
                    })
                ]
            })
        });
    }
    function $_({ entry: n }) {
        const r = Tt((d)=>d.gameState?.objects)?.[n.source_id]?.name ?? "Unknown", u = n.kind.type === "Spell";
        return S.jsxs(ge.div, {
            layout: !0,
            initial: {
                opacity: 0,
                x: 20
            },
            animate: {
                opacity: 1,
                x: 0
            },
            exit: {
                opacity: 0,
                x: -20
            },
            className: "flex items-center gap-2 rounded border border-gray-600 bg-gray-800 p-1.5",
            children: [
                u ? S.jsx(Td, {
                    cardName: r,
                    size: "small",
                    className: "!h-10 !w-7"
                }) : S.jsx("div", {
                    className: "flex h-10 w-7 items-center justify-center rounded bg-purple-900 text-xs font-bold text-purple-300",
                    children: "Ab"
                }),
                S.jsxs("div", {
                    className: "min-w-0 flex-1",
                    children: [
                        S.jsx("div", {
                            className: "truncate text-xs font-medium text-gray-100",
                            children: r
                        }),
                        S.jsxs("div", {
                            className: "text-[10px] text-gray-400",
                            children: [
                                u ? "Spell" : n.kind.type === "ActivatedAbility" ? "Activated" : "Triggered",
                                " - P",
                                n.controller + 1
                            ]
                        })
                    ]
                })
            ]
        });
    }
    function J_() {
        const n = Tt((a)=>a.gameState?.stack ?? []);
        return S.jsxs("div", {
            className: "flex flex-col gap-1",
            children: [
                S.jsx("h3", {
                    className: "text-xs font-semibold uppercase tracking-wider text-gray-400",
                    children: "Stack"
                }),
                n.length === 0 ? S.jsx("p", {
                    className: "text-xs italic text-gray-600",
                    children: "Stack empty"
                }) : S.jsx("div", {
                    className: "flex flex-col gap-1",
                    children: S.jsx(Nn, {
                        mode: "popLayout",
                        children: n.map((a)=>S.jsx($_, {
                                entry: a
                            }, a.id))
                    })
                })
            ]
        });
    }
    function W_({ from: n, to: a }) {
        const l = a.x - n.x, r = a.y - n.y, u = Math.sqrt(l * l + r * r);
        return S.jsxs("svg", {
            className: "pointer-events-none fixed inset-0 z-50",
            width: "100%",
            height: "100%",
            children: [
                S.jsx("defs", {
                    children: S.jsx("marker", {
                        id: "arrowhead",
                        markerWidth: "8",
                        markerHeight: "6",
                        refX: "8",
                        refY: "3",
                        orient: "auto",
                        children: S.jsx("path", {
                            d: "M0,0 L8,3 L0,6 Z",
                            fill: "rgba(0,229,255,0.8)"
                        })
                    })
                }),
                S.jsx(ge.line, {
                    x1: n.x,
                    y1: n.y,
                    x2: a.x,
                    y2: a.y,
                    stroke: "rgba(0,229,255,0.6)",
                    strokeWidth: 2.5,
                    markerEnd: "url(#arrowhead)",
                    initial: {
                        pathLength: 0,
                        opacity: 0
                    },
                    animate: {
                        pathLength: 1,
                        opacity: 1
                    },
                    transition: {
                        duration: Math.min(u / 800, .4),
                        ease: "easeOut"
                    }
                })
            ]
        });
    }
    function I_() {
        const n = Tt((E)=>E.waitingFor), a = Tt((E)=>E.gameState), l = Tt((E)=>E.dispatch), r = It((E)=>E.targetingMode), u = It((E)=>E.selectedTargets), d = It((E)=>E.sourceObjectId), f = It((E)=>E.startTargeting), h = It((E)=>E.clearTargets), m = T.useRef(null), p = n?.type === "TargetSelection", y = p ? n.data.pending_cast : null;
        T.useEffect(()=>{
            if (!p || !a) return;
            const E = a.battlefield.slice(), M = y?.object_id ?? null;
            return f(E, M), ()=>{
                h();
            };
        }, [
            p,
            a,
            y,
            f,
            h
        ]), T.useEffect(()=>{
            if (!d) {
                m.current = null;
                return;
            }
            const E = document.querySelector(`[data-object-id="${d}"]`);
            if (E) {
                const M = E.getBoundingClientRect();
                m.current = {
                    x: M.left + M.width / 2,
                    y: M.top + M.height / 2
                };
            }
        }, [
            d
        ]);
        const v = T.useCallback(()=>{
            const E = u.map((M)=>({
                    Object: M
                }));
            l({
                type: "SelectTargets",
                data: {
                    targets: E
                }
            }), h();
        }, [
            u,
            l,
            h
        ]), x = T.useCallback(()=>{
            h(), l({
                type: "PassPriority"
            });
        }, [
            h,
            l
        ]), A = (E)=>{
            const M = document.querySelector(`[data-object-id="${E}"]`);
            if (!M) return null;
            const R = M.getBoundingClientRect();
            return {
                x: R.left + R.width / 2,
                y: R.top + R.height / 2
            };
        };
        return !r || !p ? null : S.jsx(Nn, {
            children: S.jsxs(ge.div, {
                className: "fixed inset-0 z-40",
                initial: {
                    opacity: 0
                },
                animate: {
                    opacity: 1
                },
                exit: {
                    opacity: 0
                },
                transition: {
                    duration: .2
                },
                children: [
                    S.jsx("div", {
                        className: "pointer-events-none absolute inset-0 bg-black/30"
                    }),
                    S.jsx("div", {
                        className: "pointer-events-none absolute left-0 right-0 top-4 flex justify-center",
                        children: S.jsx("div", {
                            className: "rounded-lg bg-gray-900/90 px-6 py-2 text-lg font-semibold text-cyan-400 shadow-lg",
                            children: "Choose a target"
                        })
                    }),
                    S.jsxs("div", {
                        className: "absolute bottom-6 left-0 right-0 flex justify-center gap-4",
                        children: [
                            u.length > 0 && S.jsx("button", {
                                onClick: v,
                                className: "rounded-lg bg-cyan-600 px-6 py-2 font-semibold text-white shadow-lg transition hover:bg-cyan-500",
                                children: "Confirm Target"
                            }),
                            S.jsx("button", {
                                onClick: x,
                                className: "rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600",
                                children: "Cancel"
                            })
                        ]
                    }),
                    m.current && u.map((E)=>{
                        const M = A(E);
                        return !M || !m.current ? null : S.jsx(W_, {
                            from: m.current,
                            to: M
                        }, E);
                    })
                ]
            })
        });
    }
    function tR() {
        return Yt.create_initial_state();
    }
    function eR() {
        return Yt.get_game_state();
    }
    function nR(n) {
        return Yt.initialize_game(n);
    }
    function aR() {
        let n, a;
        try {
            const l = Yt.ping();
            return n = l[0], a = l[1], Hr(l[0], l[1]);
        } finally{
            Yt.__wbindgen_free(n, a, 1);
        }
    }
    function iR(n) {
        return Yt.submit_action(n);
    }
    function lR() {
        return {
            __proto__: null,
            "./engine_wasm_bg.js": {
                __proto__: null,
                __wbg_Error_83742b46f01ce22d: function() {
                    return wt(function(a, l) {
                        return Error(Hr(a, l));
                    }, arguments);
                },
                __wbg_Number_a5a435bd7bbec835: function() {
                    return wt(function(a) {
                        return Number(a);
                    }, arguments);
                },
                __wbg___wbindgen_bigint_get_as_i64_447a76b5c6ef7bda: function(a, l) {
                    const r = l, u = typeof r == "bigint" ? r : void 0;
                    Rn(u) || rR(u), la().setBigInt64(a + 8, Rn(u) ? BigInt(0) : u, !0), la().setInt32(a + 0, !Rn(u), !0);
                },
                __wbg___wbindgen_boolean_get_c0f3f60bac5a78d1: function(a) {
                    const l = a, r = typeof l == "boolean" ? l : void 0;
                    return Rn(r) || he(r), Rn(r) ? 16777215 : r ? 1 : 0;
                },
                __wbg___wbindgen_debug_string_5398f5bb970e0daa: function(a, l) {
                    const r = Df(l), u = y0(r, Yt.__wbindgen_malloc, Yt.__wbindgen_realloc), d = Qr;
                    la().setInt32(a + 4, d, !0), la().setInt32(a + 0, u, !0);
                },
                __wbg___wbindgen_in_41dbb8413020e076: function(a, l) {
                    const r = a in l;
                    return he(r), r;
                },
                __wbg___wbindgen_is_bigint_e2141d4f045b7eda: function(a) {
                    const l = typeof a == "bigint";
                    return he(l), l;
                },
                __wbg___wbindgen_is_function_3c846841762788c1: function(a) {
                    const l = typeof a == "function";
                    return he(l), l;
                },
                __wbg___wbindgen_is_object_781bc9f159099513: function(a) {
                    const l = a, r = typeof l == "object" && l !== null;
                    return he(r), r;
                },
                __wbg___wbindgen_is_string_7ef6b97b02428fae: function(a) {
                    const l = typeof a == "string";
                    return he(l), l;
                },
                __wbg___wbindgen_is_undefined_52709e72fb9f179c: function(a) {
                    const l = a === void 0;
                    return he(l), l;
                },
                __wbg___wbindgen_jsval_eq_ee31bfad3e536463: function(a, l) {
                    const r = a === l;
                    return he(r), r;
                },
                __wbg___wbindgen_jsval_loose_eq_5bcc3bed3c69e72b: function(a, l) {
                    const r = a == l;
                    return he(r), r;
                },
                __wbg___wbindgen_number_get_34bb9d9dcfa21373: function(a, l) {
                    const r = l, u = typeof r == "number" ? r : void 0;
                    Rn(u) || Ic(u), la().setFloat64(a + 8, Rn(u) ? 0 : u, !0), la().setInt32(a + 0, !Rn(u), !0);
                },
                __wbg___wbindgen_string_get_395e606bd0ee4427: function(a, l) {
                    const r = l, u = typeof r == "string" ? r : void 0;
                    var d = Rn(u) ? 0 : y0(u, Yt.__wbindgen_malloc, Yt.__wbindgen_realloc), f = Qr;
                    la().setInt32(a + 4, f, !0), la().setInt32(a + 0, d, !0);
                },
                __wbg___wbindgen_throw_6ddd609b62940d55: function(a, l) {
                    throw new Error(Hr(a, l));
                },
                __wbg_call_e133b57c9155d22c: function() {
                    return tf(function(a, l) {
                        return a.call(l);
                    }, arguments);
                },
                __wbg_done_08ce71ee07e3bd17: function() {
                    return wt(function(a) {
                        const l = a.done;
                        return he(l), l;
                    }, arguments);
                },
                __wbg_entries_e8a20ff8c9757101: function() {
                    return wt(function(a) {
                        return Object.entries(a);
                    }, arguments);
                },
                __wbg_get_326e41e095fb2575: function() {
                    return tf(function(a, l) {
                        return Reflect.get(a, l);
                    }, arguments);
                },
                __wbg_get_a8ee5c45dabc1b3b: function() {
                    return wt(function(a, l) {
                        return a[l >>> 0];
                    }, arguments);
                },
                __wbg_get_unchecked_329cfe50afab7352: function() {
                    return wt(function(a, l) {
                        return a[l >>> 0];
                    }, arguments);
                },
                __wbg_get_with_ref_key_6412cf3094599694: function() {
                    return wt(function(a, l) {
                        return a[l];
                    }, arguments);
                },
                __wbg_instanceof_ArrayBuffer_101e2bf31071a9f6: function() {
                    return wt(function(a) {
                        let l;
                        try {
                            l = a instanceof ArrayBuffer;
                        } catch  {
                            l = !1;
                        }
                        const r = l;
                        return he(r), r;
                    }, arguments);
                },
                __wbg_instanceof_Map_f194b366846aca0c: function() {
                    return wt(function(a) {
                        let l;
                        try {
                            l = a instanceof Map;
                        } catch  {
                            l = !1;
                        }
                        const r = l;
                        return he(r), r;
                    }, arguments);
                },
                __wbg_instanceof_Uint8Array_740438561a5b956d: function() {
                    return wt(function(a) {
                        let l;
                        try {
                            l = a instanceof Uint8Array;
                        } catch  {
                            l = !1;
                        }
                        const r = l;
                        return he(r), r;
                    }, arguments);
                },
                __wbg_isArray_33b91feb269ff46e: function() {
                    return wt(function(a) {
                        const l = Array.isArray(a);
                        return he(l), l;
                    }, arguments);
                },
                __wbg_isSafeInteger_ecd6a7f9c3e053cd: function() {
                    return wt(function(a) {
                        const l = Number.isSafeInteger(a);
                        return he(l), l;
                    }, arguments);
                },
                __wbg_iterator_d8f549ec8fb061b1: function() {
                    return wt(function() {
                        return Symbol.iterator;
                    }, arguments);
                },
                __wbg_length_b3416cf66a5452c8: function() {
                    return wt(function(a) {
                        const l = a.length;
                        return Ic(l), l;
                    }, arguments);
                },
                __wbg_length_ea16607d7b61445b: function() {
                    return wt(function(a) {
                        const l = a.length;
                        return Ic(l), l;
                    }, arguments);
                },
                __wbg_new_49d5571bd3f0c4d4: function() {
                    return wt(function() {
                        return new Map;
                    }, arguments);
                },
                __wbg_new_5f486cdf45a04d78: function() {
                    return wt(function(a) {
                        return new Uint8Array(a);
                    }, arguments);
                },
                __wbg_new_a70fbab9066b301f: function() {
                    return wt(function() {
                        return new Array;
                    }, arguments);
                },
                __wbg_new_ab79df5bd7c26067: function() {
                    return wt(function() {
                        return new Object;
                    }, arguments);
                },
                __wbg_next_11b99ee6237339e3: function() {
                    return tf(function(a) {
                        return a.next();
                    }, arguments);
                },
                __wbg_next_e01a967809d1aa68: function() {
                    return wt(function(a) {
                        return a.next;
                    }, arguments);
                },
                __wbg_prototypesetcall_d62e5099504357e6: function() {
                    return wt(function(a, l, r) {
                        Uint8Array.prototype.set.call(oR(a, l), r);
                    }, arguments);
                },
                __wbg_set_282384002438957f: function() {
                    return wt(function(a, l, r) {
                        a[l >>> 0] = r;
                    }, arguments);
                },
                __wbg_set_6be42768c690e380: function() {
                    return wt(function(a, l, r) {
                        a[l] = r;
                    }, arguments);
                },
                __wbg_set_bf7251625df30a02: function() {
                    return wt(function(a, l, r) {
                        return a.set(l, r);
                    }, arguments);
                },
                __wbg_value_21fc78aab0322612: function() {
                    return wt(function(a) {
                        return a.value;
                    }, arguments);
                },
                __wbindgen_cast_0000000000000001: function() {
                    return wt(function(a) {
                        return a;
                    }, arguments);
                },
                __wbindgen_cast_0000000000000002: function() {
                    return wt(function(a) {
                        return a;
                    }, arguments);
                },
                __wbindgen_cast_0000000000000003: function() {
                    return wt(function(a, l) {
                        return Hr(a, l);
                    }, arguments);
                },
                __wbindgen_cast_0000000000000004: function() {
                    return wt(function(a) {
                        return BigInt.asUintN(64, a);
                    }, arguments);
                },
                __wbindgen_init_externref_table: function() {
                    const a = Yt.__wbindgen_externrefs, l = a.grow(4);
                    a.set(0, void 0), a.set(l + 0, void 0), a.set(l + 1, null), a.set(l + 2, !0), a.set(l + 3, !1);
                }
            }
        };
    }
    function sR(n) {
        const a = Yt.__externref_table_alloc();
        return Yt.__wbindgen_externrefs.set(a, n), a;
    }
    function rR(n) {
        if (typeof n != "bigint") throw new Error(`expected a bigint argument, found ${typeof n}`);
    }
    function he(n) {
        if (typeof n != "boolean") throw new Error(`expected a boolean argument, found ${typeof n}`);
    }
    function Ic(n) {
        if (typeof n != "number") throw new Error(`expected a number argument, found ${typeof n}`);
    }
    function Df(n) {
        const a = typeof n;
        if (a == "number" || a == "boolean" || n == null) return `${n}`;
        if (a == "string") return `"${n}"`;
        if (a == "symbol") {
            const u = n.description;
            return u == null ? "Symbol" : `Symbol(${u})`;
        }
        if (a == "function") {
            const u = n.name;
            return typeof u == "string" && u.length > 0 ? `Function(${u})` : "Function";
        }
        if (Array.isArray(n)) {
            const u = n.length;
            let d = "[";
            u > 0 && (d += Df(n[0]));
            for(let f = 1; f < u; f++)d += ", " + Df(n[f]);
            return d += "]", d;
        }
        const l = /\[object ([^\]]+)\]/.exec(toString.call(n));
        let r;
        if (l && l.length > 1) r = l[1];
        else return toString.call(n);
        if (r == "Object") try {
            return "Object(" + JSON.stringify(n) + ")";
        } catch  {
            return "Object";
        }
        return n instanceof Error ? `${n.name}: ${n.message}
${n.stack}` : r;
    }
    function oR(n, a) {
        return n = n >>> 0, Gl().subarray(n / 1, n / 1 + a);
    }
    let Oa = null;
    function la() {
        return (Oa === null || Oa.buffer.detached === !0 || Oa.buffer.detached === void 0 && Oa.buffer !== Yt.memory.buffer) && (Oa = new DataView(Yt.memory.buffer)), Oa;
    }
    function Hr(n, a) {
        return n = n >>> 0, cR(n, a);
    }
    let zl = null;
    function Gl() {
        return (zl === null || zl.byteLength === 0) && (zl = new Uint8Array(Yt.memory.buffer)), zl;
    }
    function tf(n, a) {
        try {
            return n.apply(this, a);
        } catch (l) {
            const r = sR(l);
            Yt.__wbindgen_exn_store(r);
        }
    }
    function Rn(n) {
        return n == null;
    }
    function wt(n, a) {
        try {
            return n.apply(this, a);
        } catch (l) {
            let r = (function() {
                try {
                    return l instanceof Error ? `${l.message}

Stack:
${l.stack}` : l.toString();
                } catch  {
                    return "<failed to stringify thrown value>";
                }
            })();
            throw console.error("wasm-bindgen: imported JS function that was not marked as `catch` threw an error:", r), l;
        }
    }
    function y0(n, a, l) {
        if (typeof n != "string") throw new Error(`expected a string argument, found ${typeof n}`);
        if (l === void 0) {
            const h = ql.encode(n), m = a(h.length, 1) >>> 0;
            return Gl().subarray(m, m + h.length).set(h), Qr = h.length, m;
        }
        let r = n.length, u = a(r, 1) >>> 0;
        const d = Gl();
        let f = 0;
        for(; f < r; f++){
            const h = n.charCodeAt(f);
            if (h > 127) break;
            d[u + f] = h;
        }
        if (f !== r) {
            f !== 0 && (n = n.slice(f)), u = l(u, r, r = f + n.length * 3, 1) >>> 0;
            const h = Gl().subarray(u + f, u + r), m = ql.encodeInto(n, h);
            if (m.read !== n.length) throw new Error("failed to pass whole string");
            f += m.written, u = l(u, r, f, 1) >>> 0;
        }
        return Qr = f, u;
    }
    let Gr = new TextDecoder("utf-8", {
        ignoreBOM: !0,
        fatal: !0
    });
    Gr.decode();
    const uR = 2146435072;
    let ef = 0;
    function cR(n, a) {
        return ef += a, ef >= uR && (Gr = new TextDecoder("utf-8", {
            ignoreBOM: !0,
            fatal: !0
        }), Gr.decode(), ef = a), Gr.decode(Gl().subarray(n, n + a));
    }
    const ql = new TextEncoder;
    "encodeInto" in ql || (ql.encodeInto = function(n, a) {
        const l = ql.encode(n);
        return a.set(l), {
            read: n.length,
            written: l.length
        };
    });
    let Qr = 0, Yt;
    function fR(n, a) {
        return Yt = n.exports, Oa = null, zl = null, Yt.__wbindgen_start(), Yt;
    }
    async function dR(n, a) {
        if (typeof Response == "function" && n instanceof Response) {
            if (typeof WebAssembly.instantiateStreaming == "function") try {
                return await WebAssembly.instantiateStreaming(n, a);
            } catch (u) {
                if (n.ok && l(n.type) && n.headers.get("Content-Type") !== "application/wasm") console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", u);
                else throw u;
            }
            const r = await n.arrayBuffer();
            return await WebAssembly.instantiate(r, a);
        } else {
            const r = await WebAssembly.instantiate(n, a);
            return r instanceof WebAssembly.Instance ? {
                instance: r,
                module: n
            } : r;
        }
        function l(r) {
            switch(r){
                case "basic":
                case "cors":
                case "default":
                    return !0;
            }
            return !1;
        }
    }
    async function hR(n) {
        if (Yt !== void 0) return Yt;
        n !== void 0 && (Object.getPrototypeOf(n) === Object.prototype ? { module_or_path: n } = n : console.warn("using deprecated parameters for the initialization function; pass a single object instead")), n === void 0 && (n = new URL("/assets/engine_wasm_bg-BPIKOF4_.wasm", import.meta.url));
        const a = lR();
        (typeof n == "string" || typeof Request == "function" && n instanceof Request || typeof URL == "function" && n instanceof URL) && (n = fetch(n));
        const { instance: l, module: r } = await dR(await n, a);
        return fR(l);
    }
    class _r extends Error {
        code;
        recoverable;
        constructor(a, l, r){
            super(l), this.name = "AdapterError", this.code = a, this.recoverable = r;
        }
    }
    const nf = {
        NOT_INITIALIZED: "NOT_INITIALIZED",
        WASM_ERROR: "WASM_ERROR",
        INVALID_ACTION: "INVALID_ACTION"
    };
    class mR {
        initialized = !1;
        queue = Promise.resolve();
        async initialize() {
            this.initialized || (await hR(), this.initialized = !0);
        }
        async submitAction(a) {
            return this.assertInitialized(), this.enqueue(()=>this.processAction(a));
        }
        async getState() {
            return this.assertInitialized(), this.enqueue(()=>this.fetchState());
        }
        dispose() {
            this.initialized = !1, this.queue = Promise.resolve();
        }
        ping() {
            return this.assertInitialized(), aR();
        }
        initializeGame(a) {
            return this.assertInitialized(), nR(a ?? null).events ?? [];
        }
        assertInitialized() {
            if (!this.initialized) throw new _r(nf.NOT_INITIALIZED, "Adapter not initialized. Call initialize() first.", !0);
        }
        enqueue(a) {
            const l = this.queue.then(()=>{
                try {
                    return a();
                } catch (r) {
                    throw this.normalizeError(r);
                }
            });
            return this.queue = l.then(()=>{}, ()=>{}), l;
        }
        processAction(a) {
            const l = iR(a);
            if (typeof l == "string") throw new _r(nf.INVALID_ACTION, l, !0);
            return l.events ?? [];
        }
        fetchState() {
            const a = eR();
            return a === null ? tR() : a;
        }
        normalizeError(a) {
            if (a instanceof _r) return a;
            const l = a instanceof Error ? a.message : String(a);
            return new _r(nf.WASM_ERROR, l, !1);
        }
    }
    function pR() {
        const n = Tt((l)=>l.dispatch), a = Nl((l)=>l.enqueueEffects);
        return T.useCallback(async (l)=>{
            const r = await n(l);
            r.length > 0 && a(r);
        }, [
            n,
            a
        ]);
    }
    function gR() {
        T.useEffect(()=>{
            const n = (a)=>{
                const l = a.target;
                if (l.tagName === "INPUT" || l.tagName === "TEXTAREA" || l.tagName === "SELECT" || l.isContentEditable) return;
                const { gameState: r, waitingFor: u, dispatch: d, undo: f, stateHistory: h } = Tt.getState(), m = It.getState();
                switch(a.key){
                    case " ":
                    case "Enter":
                        u?.type === "Priority" && (a.preventDefault(), d({
                            type: "PassPriority"
                        }));
                        break;
                    case "f":
                    case "F":
                        a.preventDefault(), m.toggleFullControl();
                        break;
                    case "z":
                    case "Z":
                        !a.ctrlKey && !a.metaKey && (a.preventDefault(), h.length > 0 && f());
                        break;
                    case "t":
                    case "T":
                        if (u?.type === "ManaPayment" && r) {
                            a.preventDefault();
                            const p = r.players[r.priority_player];
                            if (p) for (const y of r.battlefield){
                                const v = r.objects[y];
                                v && v.controller === p.id && !v.tapped && v.card_types.core_types.includes("Land") && d({
                                    type: "TapLandForMana",
                                    data: {
                                        object_id: v.id
                                    }
                                });
                            }
                        }
                        break;
                    case "Escape":
                        a.preventDefault(), m.clearTargets();
                        break;
                }
            };
            return window.addEventListener("keydown", n), ()=>window.removeEventListener("keydown", n);
        }, []);
    }
    function yR() {
        const n = Tt((y)=>y.initGame), a = Tt((y)=>y.gameState), l = Tt((y)=>y.waitingFor), r = pR(), u = Tt((y)=>y.reset), d = It((y)=>y.inspectedObjectId), f = a?.objects, h = d != null && f ? f[d]?.name ?? null : null;
        gR(), T.useEffect(()=>{
            const y = new mR;
            return n(y), ()=>{
                u();
            };
        }, [
            n,
            u
        ]);
        const m = T.useCallback((y)=>{
            r({
                type: "MulliganDecision",
                data: {
                    keep: y === "keep"
                }
            });
        }, [
            r
        ]), p = T.useCallback((y)=>{
            const v = y.split(",").map(Number).filter(Boolean);
            r({
                type: "SelectCards",
                data: {
                    cards: v
                }
            });
        }, [
            r
        ]);
        return S.jsxs("div", {
            className: "flex h-screen bg-gray-950",
            children: [
                S.jsxs("div", {
                    className: "flex flex-1 flex-col overflow-hidden",
                    children: [
                        S.jsx(G_, {}),
                        S.jsx(N_, {}),
                        S.jsx(q_, {})
                    ]
                }),
                S.jsxs("div", {
                    className: "flex w-64 flex-col gap-3 border-l border-gray-800 bg-gray-900/50 p-3 lg:w-72",
                    children: [
                        S.jsx(g0, {
                            playerId: 1
                        }),
                        S.jsx(H_, {}),
                        S.jsx(J_, {}),
                        S.jsx(Y_, {}),
                        S.jsx(g0, {
                            playerId: 0
                        }),
                        S.jsxs("div", {
                            className: "flex items-center gap-2",
                            children: [
                                S.jsx(B_, {}),
                                S.jsx(V_, {})
                            ]
                        })
                    ]
                }),
                S.jsx(g_, {}),
                S.jsx(z_, {
                    cardName: h
                }),
                l?.type === "TargetSelection" && S.jsx(I_, {}),
                l?.type === "ManaPayment" && S.jsx(Z_, {}),
                l?.type === "ReplacementChoice" && S.jsx(F_, {}),
                l?.type === "MulliganDecision" && S.jsx(Q_, {
                    title: `Mulligan (${l.data.mulligan_count} cards)`,
                    options: [
                        {
                            id: "keep",
                            label: "Keep Hand"
                        },
                        {
                            id: "mulligan",
                            label: "Mulligan",
                            description: `Draw ${7 - l.data.mulligan_count - 1} cards`
                        }
                    ],
                    onChoose: m
                }),
                l?.type === "MulliganBottomCards" && S.jsx(vR, {
                    playerId: l.data.player,
                    count: l.data.count,
                    onChoose: p
                }),
                l?.type === "GameOver" && S.jsx(bR, {
                    winner: l.data.winner
                }),
                S.jsx("style", {
                    children: `
        @media (max-width: 768px) {
          .flex.h-screen {
            flex-direction: column;
          }
          .flex.h-screen > .w-64,
          .flex.h-screen > .lg\\:w-72 {
            width: 100%;
            max-height: 40vh;
            border-left: none;
            border-top: 1px solid rgb(31 41 55);
          }
        }
      `
                })
            ]
        });
    }
    function vR({ playerId: n, count: a, onChoose: l }) {
        const r = Tt((y)=>y.gameState?.players[n]), u = Tt((y)=>y.gameState?.objects), d = It((y)=>y.selectedTargets), f = It((y)=>y.addTarget);
        if (!r || !u) return null;
        const h = r.hand.map((y)=>u[y]).filter(Boolean), m = d.length === a, p = ()=>{
            l(d.join(","));
        };
        return S.jsxs("div", {
            className: "fixed inset-0 z-50 flex items-center justify-center",
            children: [
                S.jsx("div", {
                    className: "absolute inset-0 bg-black/60"
                }),
                S.jsxs("div", {
                    className: "relative z-10 w-full max-w-lg rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700",
                    children: [
                        S.jsxs("h2", {
                            className: "mb-2 text-center text-lg font-bold text-white",
                            children: [
                                "Put ",
                                a,
                                " card",
                                a > 1 ? "s" : "",
                                " on bottom"
                            ]
                        }),
                        S.jsxs("p", {
                            className: "mb-4 text-center text-sm text-gray-400",
                            children: [
                                "Select ",
                                a,
                                " card",
                                a > 1 ? "s" : "",
                                " to put on the bottom of your library"
                            ]
                        }),
                        S.jsx("div", {
                            className: "mb-4 flex flex-wrap justify-center gap-2",
                            children: h.map((y)=>{
                                const v = d.includes(y.id);
                                return S.jsx("button", {
                                    onClick: ()=>{
                                        !v && d.length < a && f(y.id);
                                    },
                                    className: `rounded-lg px-3 py-2 text-sm transition ${v ? "bg-cyan-600 text-white ring-2 ring-cyan-400" : "bg-gray-800 text-gray-300 hover:bg-gray-700"}`,
                                    children: y.name
                                }, y.id);
                            })
                        }),
                        S.jsx("div", {
                            className: "flex justify-center",
                            children: S.jsxs("button", {
                                onClick: p,
                                disabled: !m,
                                className: `rounded-lg px-6 py-2 font-semibold transition ${m ? "bg-cyan-600 text-white hover:bg-cyan-500" : "cursor-not-allowed bg-gray-700 text-gray-500"}`,
                                children: [
                                    "Confirm (",
                                    d.length,
                                    "/",
                                    a,
                                    ")"
                                ]
                            })
                        })
                    ]
                })
            ]
        });
    }
    function bR({ winner: n }) {
        return S.jsxs("div", {
            className: "fixed inset-0 z-50 flex items-center justify-center",
            children: [
                S.jsx("div", {
                    className: "absolute inset-0 bg-black/70"
                }),
                S.jsxs("div", {
                    className: "relative z-10 rounded-xl bg-gray-900 p-8 text-center shadow-2xl ring-1 ring-gray-700",
                    children: [
                        S.jsx("h2", {
                            className: "mb-2 text-2xl font-bold text-white",
                            children: "Game Over"
                        }),
                        S.jsx("p", {
                            className: "text-lg text-gray-300",
                            children: n != null ? n === 0 ? "You Win!" : "Opponent Wins" : "Draw"
                        })
                    ]
                })
            ]
        });
    }
    const xR = 300, SR = [
        "W",
        "U",
        "B",
        "R",
        "G"
    ], TR = {
        W: "White",
        U: "Blue",
        B: "Black",
        R: "Red",
        G: "Green"
    }, ER = {
        W: "bg-amber-100 text-amber-900",
        U: "bg-blue-500 text-white",
        B: "bg-gray-800 text-gray-100",
        R: "bg-red-600 text-white",
        G: "bg-green-600 text-white"
    }, AR = [
        "Creature",
        "Instant",
        "Sorcery",
        "Enchantment",
        "Artifact",
        "Land",
        "Planeswalker"
    ];
    function CR({ onResults: n }) {
        const [a, l] = T.useState(""), [r, u] = T.useState([]), [d, f] = T.useState(""), [h, m] = T.useState(void 0), [p, y] = T.useState(!1), [v, x] = T.useState(null), [A, E] = T.useState(null), M = T.useRef(null), R = T.useRef(null), z = T.useCallback(async (H, Z, Q, it)=>{
            M.current?.abort();
            const bt = __({
                text: H || void 0,
                colors: Z.length > 0 ? Z : void 0,
                type: Q || void 0,
                cmcMax: it,
                format: "standard"
            });
            if (!bt || bt === "f:standard") {
                n([], 0), x(null);
                return;
            }
            const gt = new AbortController;
            M.current = gt, y(!0), E(null);
            try {
                const { cards: Nt, total: ee } = await Hb(bt, gt.signal);
                gt.signal.aborted || (n(Nt, ee), x(ee));
            } catch (Nt) {
                gt.signal.aborted || (E(Nt instanceof Error ? Nt.message : "Search failed"), n([], 0), x(null));
            } finally{
                gt.signal.aborted || y(!1);
            }
        }, [
            n
        ]), B = T.useCallback((H, Z, Q, it)=>{
            R.current && clearTimeout(R.current), R.current = setTimeout(()=>z(H, Z, Q, it), xR);
        }, [
            z
        ]);
        T.useEffect(()=>()=>{
                M.current?.abort(), R.current && clearTimeout(R.current);
            }, []);
        const V = (H)=>{
            l(H), B(H, r, d, h);
        }, P = (H)=>{
            const Z = r.includes(H) ? r.filter((Q)=>Q !== H) : [
                ...r,
                H
            ];
            u(Z), B(a, Z, d, h);
        }, U = (H)=>{
            f(H), B(a, r, H, h);
        }, X = (H)=>{
            const Z = H === "" ? void 0 : parseInt(H, 10);
            m(Z), B(a, r, d, Z);
        };
        return S.jsxs("div", {
            className: "flex flex-col gap-3 p-3",
            children: [
                S.jsx("input", {
                    type: "text",
                    value: a,
                    onChange: (H)=>V(H.target.value),
                    placeholder: "Search cards...",
                    className: "w-full rounded-md border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-blue-500 focus:outline-none"
                }),
                S.jsx("div", {
                    className: "flex gap-1",
                    children: SR.map((H)=>S.jsx("button", {
                            onClick: ()=>P(H),
                            title: TR[H],
                            className: `h-8 w-8 rounded-full text-xs font-bold transition-opacity ${ER[H]} ${r.includes(H) ? "opacity-100 ring-2 ring-white" : "opacity-40"}`,
                            children: H
                        }, H))
                }),
                S.jsxs("select", {
                    value: d,
                    onChange: (H)=>U(H.target.value),
                    className: "rounded-md border border-gray-700 bg-gray-800 px-3 py-1.5 text-sm text-white focus:border-blue-500 focus:outline-none",
                    children: [
                        S.jsx("option", {
                            value: "",
                            children: "All types"
                        }),
                        AR.map((H)=>S.jsx("option", {
                                value: H,
                                children: H
                            }, H))
                    ]
                }),
                S.jsxs("div", {
                    className: "flex items-center gap-2",
                    children: [
                        S.jsx("label", {
                            className: "text-xs text-gray-400",
                            children: "CMC max:"
                        }),
                        S.jsx("input", {
                            type: "number",
                            min: 0,
                            max: 16,
                            value: h ?? "",
                            onChange: (H)=>X(H.target.value),
                            className: "w-16 rounded-md border border-gray-700 bg-gray-800 px-2 py-1 text-sm text-white focus:border-blue-500 focus:outline-none"
                        })
                    ]
                }),
                S.jsxs("div", {
                    className: "text-xs text-gray-400",
                    children: [
                        p && "Searching...",
                        !p && v !== null && `${v} results`,
                        A && S.jsx("span", {
                            className: "text-red-400",
                            children: A
                        })
                    ]
                })
            ]
        });
    }
    function wR(n) {
        return n.legalities?.standard === "legal";
    }
    function _R({ cards: n, onAddCard: a }) {
        return S.jsx("div", {
            className: "grid auto-rows-min grid-cols-[repeat(auto-fill,minmax(130px,1fr))] gap-2 overflow-y-auto p-2",
            children: S.jsx(Nn, {
                mode: "popLayout",
                children: n.map((l)=>{
                    const r = R_(l), u = wR(l);
                    return S.jsxs(ge.button, {
                        layout: !0,
                        initial: {
                            opacity: 0,
                            scale: .9
                        },
                        animate: {
                            opacity: 1,
                            scale: 1
                        },
                        exit: {
                            opacity: 0,
                            scale: .9
                        },
                        transition: {
                            duration: .15
                        },
                        onClick: ()=>u && a(l),
                        disabled: !u,
                        title: u ? `Add ${l.name}` : `${l.name} - Not Standard legal`,
                        className: `group relative cursor-pointer overflow-hidden rounded-lg transition-transform hover:scale-105 ${u ? "ring-2 ring-transparent hover:ring-green-500" : "cursor-not-allowed opacity-60 ring-2 ring-red-600"}`,
                        children: [
                            r ? S.jsx("img", {
                                src: r,
                                alt: l.name,
                                className: "aspect-[488/680] w-full rounded-lg object-cover",
                                loading: "lazy"
                            }) : S.jsx("div", {
                                className: "flex aspect-[488/680] w-full items-center justify-center rounded-lg bg-gray-800 text-xs text-gray-400",
                                children: l.name
                            }),
                            !u && S.jsx("div", {
                                className: "absolute inset-0 flex items-center justify-center bg-black/50",
                                children: S.jsx("span", {
                                    className: "rounded bg-red-700 px-2 py-0.5 text-[10px] font-bold text-white",
                                    children: "Not Standard"
                                })
                            }),
                            S.jsx("div", {
                                className: "pointer-events-none absolute bottom-0 left-0 right-0 translate-y-full bg-black/80 px-1.5 py-1 text-[10px] text-white transition-transform group-hover:translate-y-0",
                                children: l.name
                            })
                        ]
                    }, l.id);
                })
            })
        });
    }
    function RR(n) {
        const a = n.split(/\r?\n/), l = {
            main: [],
            sideboard: []
        };
        let r = "main";
        for (const u of a){
            const d = u.trim();
            if (!d || d.startsWith("#")) continue;
            const f = d.match(/^\[(\w+)\]$/i);
            if (f) {
                r = f[1].toLowerCase() === "sideboard" ? "sideboard" : "main";
                continue;
            }
            const h = d.match(/^(\d+)x?\s+(.+)$/);
            h && l[r].push({
                count: parseInt(h[1], 10),
                name: h[2].trim()
            });
        }
        return l;
    }
    function MR(n) {
        const a = [];
        if (n.main.length > 0) {
            a.push("[Main]");
            for (const l of n.main)a.push(`${l.count} ${l.name}`);
        }
        if (n.sideboard.length > 0) {
            a.push("[Sideboard]");
            for (const l of n.sideboard)a.push(`${l.count} ${l.name}`);
        }
        return a.join(`
`) + `
`;
    }
    function DR(n) {
        const a = {
            Creatures: [],
            Spells: [],
            Lands: []
        };
        for (const l of n)a.Spells.push(l);
        return a;
    }
    function jf(n) {
        return n.reduce((a, l)=>a + l.count, 0);
    }
    function jR({ entry: n, section: a, onRemove: l }) {
        return S.jsxs("div", {
            className: "group flex items-center justify-between py-0.5 text-sm",
            children: [
                S.jsxs("span", {
                    className: "text-gray-300",
                    children: [
                        S.jsxs("span", {
                            className: "mr-1 text-gray-500",
                            children: [
                                n.count,
                                "x"
                            ]
                        }),
                        n.name
                    ]
                }),
                S.jsx("button", {
                    onClick: ()=>l(n.name, a),
                    className: "invisible ml-2 h-5 w-5 rounded text-xs text-red-400 hover:bg-red-900/40 group-hover:visible",
                    title: `Remove one ${n.name}`,
                    children: "-"
                })
            ]
        });
    }
    function v0({ title: n, entries: a, section: l, onRemove: r }) {
        if (a.length === 0) return null;
        const u = jf(a);
        return S.jsxs("div", {
            className: "mb-2",
            children: [
                S.jsxs("div", {
                    className: "mb-1 flex justify-between text-xs font-semibold uppercase text-gray-500",
                    children: [
                        S.jsx("span", {
                            children: n
                        }),
                        S.jsxs("span", {
                            children: [
                                "(",
                                u,
                                ")"
                            ]
                        })
                    ]
                }),
                a.map((d)=>S.jsx(jR, {
                        entry: d,
                        section: l,
                        onRemove: r
                    }, d.name))
            ]
        });
    }
    function OR({ deck: n, onRemoveCard: a, onImport: l, onExport: r }) {
        const u = T.useRef(null), d = jf(n.main), f = jf(n.sideboard), h = DR(n.main), m = [];
        d > 0 && d < 60 && m.push(`Deck has ${d} cards (minimum 60)`);
        const p = new Set([
            "Plains",
            "Island",
            "Swamp",
            "Mountain",
            "Forest"
        ]);
        for (const x of n.main)x.count > 4 && !p.has(x.name) && m.push(`${x.name}: ${x.count} copies (max 4)`);
        const y = async (x)=>{
            const A = x.target.files?.[0];
            if (!A) return;
            const E = await A.text(), M = RR(E);
            l(M), u.current && (u.current.value = "");
        }, v = ()=>{
            const x = MR(n), A = new Blob([
                x
            ], {
                type: "text/plain"
            }), E = URL.createObjectURL(A), M = document.createElement("a");
            M.href = E, M.download = "deck.dck", M.click(), URL.revokeObjectURL(E), r();
        };
        return S.jsxs("div", {
            className: "flex h-full flex-col",
            children: [
                S.jsxs("div", {
                    className: "mb-2 flex items-center justify-between border-b border-gray-700 pb-2",
                    children: [
                        S.jsxs("h3", {
                            className: "text-sm font-bold text-white",
                            children: [
                                "Deck (",
                                d,
                                " cards)"
                            ]
                        }),
                        S.jsxs("div", {
                            className: "flex gap-1",
                            children: [
                                S.jsx("button", {
                                    onClick: ()=>u.current?.click(),
                                    className: "rounded bg-gray-700 px-2 py-1 text-xs text-gray-300 hover:bg-gray-600",
                                    title: "Import .dck/.dec file",
                                    children: "Import"
                                }),
                                S.jsx("button", {
                                    onClick: v,
                                    disabled: d === 0,
                                    className: "rounded bg-gray-700 px-2 py-1 text-xs text-gray-300 hover:bg-gray-600 disabled:opacity-40",
                                    title: "Export deck as .dck file",
                                    children: "Export"
                                }),
                                S.jsx("input", {
                                    ref: u,
                                    type: "file",
                                    accept: ".dck,.dec",
                                    onChange: y,
                                    className: "hidden"
                                })
                            ]
                        })
                    ]
                }),
                m.length > 0 && S.jsx("div", {
                    className: "mb-2 space-y-0.5",
                    children: m.map((x)=>S.jsx("div", {
                            className: "rounded bg-yellow-900/30 px-2 py-1 text-xs text-yellow-400",
                            children: x
                        }, x))
                }),
                S.jsxs("div", {
                    className: "flex-1 overflow-y-auto",
                    children: [
                        [
                            "Creatures",
                            "Spells",
                            "Lands"
                        ].map((x)=>S.jsx(v0, {
                                title: x,
                                entries: h[x],
                                section: "main",
                                onRemove: a
                            }, x)),
                        n.sideboard.length > 0 && S.jsx("div", {
                            className: "mt-3 border-t border-gray-700 pt-2",
                            children: S.jsx(v0, {
                                title: `Sideboard (${f})`,
                                entries: n.sideboard,
                                section: "sideboard",
                                onRemove: a
                            })
                        })
                    ]
                })
            ]
        });
    }
    const NR = [
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6+"
    ], Rr = {
        W: {
            bg: "bg-amber-200",
            label: "W"
        },
        U: {
            bg: "bg-blue-500",
            label: "U"
        },
        B: {
            bg: "bg-gray-700",
            label: "B"
        },
        R: {
            bg: "bg-red-600",
            label: "R"
        },
        G: {
            bg: "bg-green-600",
            label: "G"
        }
    };
    function zR({ cmcValues: n, colorValues: a }) {
        const l = new Array(7).fill(0);
        for (const f of n){
            const h = Math.min(Math.floor(f), 6);
            l[h]++;
        }
        const r = Math.max(...l, 1), u = {};
        let d = 0;
        for (const f of a)for (const h of f.split(""))Rr[h] && (u[h] = (u[h] ?? 0) + 1, d++);
        return S.jsxs("div", {
            className: "space-y-3",
            children: [
                S.jsxs("div", {
                    children: [
                        S.jsx("h4", {
                            className: "mb-1 text-xs font-semibold uppercase text-gray-500",
                            children: "Mana Curve"
                        }),
                        S.jsx("div", {
                            className: "flex items-end gap-1",
                            style: {
                                height: 80
                            },
                            children: l.map((f, h)=>{
                                const m = r > 0 ? f / r * 100 : 0;
                                return S.jsxs("div", {
                                    className: "flex flex-1 flex-col items-center justify-end",
                                    children: [
                                        S.jsx("span", {
                                            className: "mb-0.5 text-[10px] text-gray-400",
                                            children: f > 0 ? f : ""
                                        }),
                                        S.jsx("div", {
                                            className: "w-full rounded-t bg-blue-500 transition-all",
                                            style: {
                                                height: `${m}%`,
                                                minHeight: f > 0 ? 4 : 0
                                            }
                                        }),
                                        S.jsx("span", {
                                            className: "mt-0.5 text-[10px] text-gray-500",
                                            children: NR[h]
                                        })
                                    ]
                                }, h);
                            })
                        })
                    ]
                }),
                d > 0 && S.jsxs("div", {
                    children: [
                        S.jsx("h4", {
                            className: "mb-1 text-xs font-semibold uppercase text-gray-500",
                            children: "Colors"
                        }),
                        S.jsx("div", {
                            className: "flex h-3 overflow-hidden rounded",
                            children: Object.entries(Rr).map(([f, { bg: h }])=>{
                                const m = u[f] ?? 0;
                                if (m === 0) return null;
                                const p = m / d * 100;
                                return S.jsx("div", {
                                    className: `${h} transition-all`,
                                    style: {
                                        width: `${p}%`
                                    },
                                    title: `${Rr[f].label}: ${Math.round(p)}%`
                                }, f);
                            })
                        }),
                        S.jsx("div", {
                            className: "mt-1 flex gap-2",
                            children: Object.entries(Rr).map(([f, { bg: h, label: m }])=>{
                                const p = u[f] ?? 0;
                                if (p === 0) return null;
                                const y = Math.round(p / d * 100);
                                return S.jsxs("span", {
                                    className: "flex items-center gap-1 text-[10px] text-gray-400",
                                    children: [
                                        S.jsx("span", {
                                            className: `inline-block h-2 w-2 rounded-full ${h}`
                                        }),
                                        m,
                                        " ",
                                        y,
                                        "%"
                                    ]
                                }, f);
                            })
                        })
                    ]
                })
            ]
        });
    }
    const Fr = "forge-deck:";
    function b0() {
        const n = [];
        for(let a = 0; a < localStorage.length; a++){
            const l = localStorage.key(a);
            l?.startsWith(Fr) && n.push(l.slice(Fr.length));
        }
        return n.sort();
    }
    function LR() {
        const n = Lf(), [a, l] = T.useState({
            main: [],
            sideboard: []
        }), [r, u] = T.useState([]), [d, f] = T.useState(""), [h, m] = T.useState(b0), [p, y] = T.useState(new Map), v = T.useCallback((U, X)=>{
            u(U), y((H)=>{
                const Z = new Map(H);
                for (const Q of U)Z.set(Q.name, Q);
                return Z;
            });
        }, []), x = T.useCallback((U)=>{
            y((X)=>new Map(X).set(U.name, U)), l((X)=>{
                const H = X.main.find((Q)=>Q.name === U.name), Z = new Set([
                    "Plains",
                    "Island",
                    "Swamp",
                    "Mountain",
                    "Forest"
                ]);
                return H && H.count >= 4 && !Z.has(U.name) ? X : H ? {
                    ...X,
                    main: X.main.map((Q)=>Q.name === U.name ? {
                            ...Q,
                            count: Q.count + 1
                        } : Q)
                } : {
                    ...X,
                    main: [
                        ...X.main,
                        {
                            count: 1,
                            name: U.name
                        }
                    ]
                };
            });
        }, []), A = T.useCallback((U, X)=>{
            l((H)=>{
                const Z = H[X], Q = Z.find((it)=>it.name === U);
                return Q ? Q.count <= 1 ? {
                    ...H,
                    [X]: Z.filter((it)=>it.name !== U)
                } : {
                    ...H,
                    [X]: Z.map((it)=>it.name === U ? {
                            ...it,
                            count: it.count - 1
                        } : it)
                } : H;
            });
        }, []), E = T.useCallback((U)=>{
            l(U);
        }, []), M = T.useCallback(()=>{}, []), R = ()=>{
            if (!d.trim()) return;
            const U = JSON.stringify(a);
            localStorage.setItem(Fr + d.trim(), U), m(b0());
        }, z = (U)=>{
            const X = localStorage.getItem(Fr + U);
            X && (l(JSON.parse(X)), f(U));
        }, B = ()=>{
            sessionStorage.setItem("forge-active-deck", JSON.stringify(a)), n("/game");
        }, V = [], P = [];
        for (const U of a.main){
            const X = p.get(U.name);
            if (X) for(let H = 0; H < U.count; H++)V.push(X.cmc), P.push(X.color_identity?.join("") ?? "");
        }
        return S.jsxs("div", {
            className: "flex h-screen flex-col bg-gray-950",
            children: [
                S.jsxs("div", {
                    className: "flex items-center justify-between border-b border-gray-800 px-4 py-2",
                    children: [
                        S.jsx("button", {
                            onClick: ()=>n("/"),
                            className: "text-sm text-gray-400 hover:text-white",
                            children: "← Menu"
                        }),
                        S.jsxs("div", {
                            className: "flex items-center gap-2",
                            children: [
                                S.jsx("input", {
                                    type: "text",
                                    value: d,
                                    onChange: (U)=>f(U.target.value),
                                    placeholder: "Deck name...",
                                    className: "w-40 rounded border border-gray-700 bg-gray-800 px-2 py-1 text-sm text-white placeholder-gray-500 focus:border-blue-500 focus:outline-none"
                                }),
                                S.jsx("button", {
                                    onClick: R,
                                    disabled: !d.trim(),
                                    className: "rounded bg-blue-600 px-3 py-1 text-sm text-white hover:bg-blue-500 disabled:opacity-40",
                                    children: "Save"
                                }),
                                h.length > 0 && S.jsxs("select", {
                                    onChange: (U)=>U.target.value && z(U.target.value),
                                    defaultValue: "",
                                    className: "rounded border border-gray-700 bg-gray-800 px-2 py-1 text-sm text-white focus:outline-none",
                                    children: [
                                        S.jsx("option", {
                                            value: "",
                                            children: "Load deck..."
                                        }),
                                        h.map((U)=>S.jsx("option", {
                                                value: U,
                                                children: U
                                            }, U))
                                    ]
                                }),
                                S.jsx("button", {
                                    onClick: B,
                                    disabled: a.main.length === 0,
                                    className: "rounded bg-green-600 px-3 py-1 text-sm font-semibold text-white hover:bg-green-500 disabled:opacity-40",
                                    children: "Start Game"
                                })
                            ]
                        })
                    ]
                }),
                S.jsxs("div", {
                    className: "flex min-h-0 flex-1",
                    children: [
                        S.jsx("div", {
                            className: "w-56 shrink-0 overflow-y-auto border-r border-gray-800",
                            children: S.jsx(CR, {
                                onResults: v
                            })
                        }),
                        S.jsx("div", {
                            className: "min-w-0 flex-1 overflow-y-auto",
                            children: S.jsx(_R, {
                                cards: r,
                                onAddCard: x
                            })
                        }),
                        S.jsxs("div", {
                            className: "w-64 shrink-0 overflow-y-auto border-l border-gray-800 p-3",
                            children: [
                                S.jsx(OR, {
                                    deck: a,
                                    onRemoveCard: A,
                                    onImport: E,
                                    onExport: M
                                }),
                                S.jsx("div", {
                                    className: "mt-3 border-t border-gray-700 pt-3",
                                    children: S.jsx(zR, {
                                        cmcValues: V,
                                        colorValues: P
                                    })
                                })
                            ]
                        })
                    ]
                })
            ]
        });
    }
    function VR() {
        return S.jsx("div", {
            className: "h-screen bg-gray-950",
            children: S.jsx(LR, {})
        });
    }
    function BR() {
        return S.jsx(kT, {
            children: S.jsx("div", {
                className: "min-h-screen bg-gray-950 text-white",
                children: S.jsxs(vT, {
                    children: [
                        S.jsx(Mr, {
                            path: "/",
                            element: S.jsx(IT, {})
                        }),
                        S.jsx(Mr, {
                            path: "/game",
                            element: S.jsx(yR, {})
                        }),
                        S.jsx(Mr, {
                            path: "/deck-builder",
                            element: S.jsx(VR, {})
                        })
                    ]
                })
            })
        });
    }
    S1.createRoot(document.getElementById("root")).render(S.jsx(T.StrictMode, {
        children: S.jsx(BR, {})
    }));
})();
