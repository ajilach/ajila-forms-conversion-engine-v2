//! JavaScript Helper Code
//!
//! This module contains JavaScript helper code that is injected into the
//! scripting environment to provide XFA-specific functionality.

/// The Form DOM's parent/child links and the XFA 3.3 §3 SOM walker.
///
/// Every node object registered by the engine is linked into its parent with
/// `_xfa_link_child_`: `child.parent` points up, and the parent keeps every
/// child under `_xfa_kids_[name][index]`, so the n-th of several same-named
/// siblings is reachable even though only index 0 is the plain property
/// `parent[name]` (XFA 3.3 §3: `Row` means `Row[0]`).
///
/// `_xfa_resolve_nodes_(start, expr)` resolves a SOM expression against those
/// links: the first segment by the scope walk (children of the current
/// container, then of each ancestor in turn, the ancestors themselves
/// included), every later segment among the children of what the previous
/// segment selected, `[n]` and `[*]` per parent. It returns `null` for syntax
/// it does not handle (`..`, `$data`, `$record`, class references `#x`,
/// predicates), so callers fall back to the name-based lookup; an expression
/// it does handle but which names nothing yields an empty array.
pub const XFA_SOM_WALKER: &str = r#"
var _xfa_global_ = (function() { return this; })();

function _xfa_hidden_(obj, key, value) {
    Object.defineProperty(obj, key, {
        value: value, writable: true, enumerable: false, configurable: true
    });
}

function _xfa_kid_list_(parent, name) {
    if (!Object.prototype.hasOwnProperty.call(parent, '_xfa_kids_')) {
        _xfa_hidden_(parent, '_xfa_kids_', {});
    }
    var list = parent._xfa_kids_[name];
    if (!list) {
        list = [];
        parent._xfa_kids_[name] = list;
    }
    return list;
}

// Link `child` into `parent` as instance `index` of `name`. A subform also
// gets its instance manager (`parent._name`, XFA 3.3 §9), created with the
// limits of a subform without `<occur>` (1/1) unless the engine installed
// the declared ones first.
// `parent.Name` is the first instance of the child `Name`. The property
// takes another node (the engine relinking an instance, the instance
// manager renumbering after a removal). Assigning anything else -- a string,
// a number, null -- sets the child's value, the way Acrobat answers
// `this.ffDesSignature = "Firma del client advisor"` (AAJB_033_IT's
// signature block, whose caption Acrobat shows exactly so; see
// xfa/acrobat_compat.rs). It never replaces the node itself: every later
// `Section.Field.rawValue` must still reach the field.
function _xfa_set_child_(parent, name, child) {
    Object.defineProperty(parent, name, {
        get: function() { return child; },
        set: function(v) {
            if (v !== null && typeof v === 'object') {
                _xfa_set_child_(parent, name, v);
            } else {
                child.rawValue = v;
            }
        },
        enumerable: true, configurable: true
    });
}

function _xfa_link_child_(parent, name, index, child, isSubform) {
    var list = _xfa_kid_list_(parent, name);
    list[index] = child;
    if (index === 0) {
        _xfa_set_child_(parent, name, child);
    }
    _xfa_hidden_(child, 'parent', parent);
    child.index = index;
    if (isSubform) {
        var manager = _xfa_manager_(parent, name, 1, 1);
        child.instanceManager = manager;
        child.instanceIndex = index;
    }
}


// Global data binding (XFA 3.3 §6 "Global data", `<bind match="global"/>`):
// one global data value node binds to every field of its name, so a value
// written to any of them is the value of all of them, the way a value
// written to the Form DOM passes through to the Data DOM and back. Each
// bound node is joined under its name; `_xfa_set_raw_` is the one write to
// a node's own value, so every write reaches the peers.
var _xfa_global_bound_ = {};

function _xfa_join_global_binding_(node, name) {
    var list = _xfa_global_bound_[name];
    if (!list) {
        list = [];
        _xfa_global_bound_[name] = list;
    }
    if (list.indexOf(node) < 0) {
        // The one data node already has a value when a later field binds
        // to it (a new row of AALU_019_DE's table joining
        // `Global_AccountNumber`): the field takes that value, as a field
        // merged with a data node does.
        for (var i = 0; i < list.length; i++) {
            if (_xfa_content_value_(list[i]._rawValue) !== null) {
                node._rawValue = list[i]._rawValue;
                break;
            }
        }
        list.push(node);
    }
    _xfa_hidden_(node, '_xfa_globalName_', name);
}

// A node's stored content as its `rawValue` reads: `null` when there is
// none (XFA 3.3 §2 "Field Values": no content is a null value; §6 "Null
// Data": empty content is null with `nullType="empty"`), the content
// otherwise.
function _xfa_content_value_(v) {
    return (v === undefined || v === null || String(v) === '') ? null : v;
}

function _xfa_set_raw_(node, v) {
    node._rawValue = v;
    var name = node._xfa_globalName_;
    if (name === undefined) return;
    var peers = _xfa_global_bound_[name];
    for (var i = 0; i < peers.length; i++) {
        if (peers[i] !== node) peers[i]._rawValue = v;
    }
}

// A plain array that also answers XFA's list protocol: `.length`, `.item(i)`.
function _xfa_collection_(arr) {
    _xfa_hidden_(arr, 'item', function(i) {
        return (i >= 0 && i < this.length) ? this[i] : null;
    });
    return arr;
}

// Every child of `obj` named `name`, in instance order.
function _xfa_kids_of_(obj, name) {
    if (obj === null || typeof obj !== 'object') return [];
    var manager = obj['_' + name];
    if (manager && manager._instances) return manager._instances.slice();
    if (obj._xfa_kids_ && obj._xfa_kids_[name]) {
        return obj._xfa_kids_[name].filter(function(k) { return !!k; });
    }
    if (!Object.prototype.hasOwnProperty.call(obj, name)) return [];
    var direct = obj[name];
    if (direct !== null && typeof direct === 'object') return [direct];
    return [];
}

// The XFA 3.3 §3 scope walk for an unqualified name: children of the
// container, then the container and its siblings (the parent's children),
// the parent and its siblings, and so on up to the root.
//
// At each level the container itself is checked before its siblings: when
// scope-matching, an unqualified name without an index takes the index of
// the container (§3 "Using relocatable calculations in a spreadsheet-like
// organization"), so a script in `Row[1]` naming `Row` means `Row[1]`, not
// `Row[0]`. A name with an index (`Row[0]`, `Row[*]`) asks for the sibling
// set instead, so `indexed` skips that rule; the root alone still answers
// to its own name.
function _xfa_scope_walk_(from, name, indexed) {
    for (var anc = from; anc; anc = anc.parent) {
        var kids = _xfa_kids_of_(anc, name);
        if (kids.length > 0) return kids;
        if (anc.name === name && (!indexed || !anc.parent)) return [anc];
    }
    return [];
}

// The `nodes` property (LiveCycle Designer Scripting Reference; XFA 3.3 §3
// "The Form DOM" defers to it): the node's children as a list. The Form DOM
// this engine keeps is sparse the way the spec says Adobe's is -- only
// instantiated nodes are listed, which here means the registered containers,
// fields and draws, not property nodes such as `<border>` or `<ui>`. Same-
// named instances are listed together, in document order of the first one.
function _xfa_nodes_of_(n) {
    var out = [];
    if (n !== null && typeof n === 'object'
        && Object.prototype.hasOwnProperty.call(n, '_xfa_kids_')) {
        for (var name in n._xfa_kids_) {
            var kids = n._xfa_kids_[name];
            for (var i = 0; i < kids.length; i++) if (kids[i]) out.push(kids[i]);
        }
    }
    return _xfa_collection_(out);
}

// The properties of the container a script names without `this.`, as
// Acrobat resolves them (`parent.index + 1` in a row's own script).
var _xfa_own_scope_props_ = {
    parent: true, index: true, instanceIndex: true, instanceManager: true,
    name: true, somExpression: true
};

// What an unqualified name in a script owned by `node` refers to, or
// `undefined` when the form has nothing by that name. A global that is not
// a form node (a JavaScript builtin such as `Date`, `xfa`, `app`, a script
// object, a value a script assigned without `var`) wins, as it always has:
// form fields named `Date` must not hide the constructor.
function _xfa_scope_value_(node, name) {
    if (typeof name !== 'string') return undefined;
    if (name.charAt(0) === '_') {
        // `_Name` is the instance manager of the subforms `Name` (XFA 3.3
        // §9), held by their parent, so it is in scope wherever `Name` is.
        for (var holder = node; holder; holder = holder.parent) {
            if (Object.prototype.hasOwnProperty.call(holder, name)) {
                var manager = holder[name];
                if (manager && typeof manager.addInstance === 'function') return manager;
            }
        }
        return undefined;
    }
    // A named script object is registered with the subform declaring it
    // (XFA 3.3 §10): the nearest declaring ancestor's own object wins over
    // the global of that name.
    if (typeof _xfa_script_objects_ !== 'undefined'
        && Object.prototype.hasOwnProperty.call(_xfa_script_objects_, name)) {
        for (var anc = node; anc; anc = anc.parent) {
            if (Object.prototype.hasOwnProperty.call(anc, name)) return anc[name];
        }
        return undefined;
    }
    if (name in _xfa_global_) {
        var g = _xfa_global_[name];
        if (!(g !== null && typeof g === 'object'
              && Object.prototype.hasOwnProperty.call(g, 'somExpression'))) {
            return undefined;
        }
    }
    if (_xfa_own_scope_props_[name] === true
        && Object.prototype.hasOwnProperty.call(node, name)) {
        var own = node[name];
        if (own !== undefined && own !== null) return own;
    }
    var found = _xfa_scope_walk_(node, name);
    return found.length > 0 ? found[0] : undefined;
}

// The scope a script owned by `node` runs in (`with` around its function,
// see the engine's `execute_javascript`): unqualified names resolve by
// `_xfa_scope_value_`; anything it does not resolve falls through to the
// global object. A node without links (a path the engine made an object for
// on the fly) uses the nearest registered node on the current path, and a
// script with no container at all runs in the form root's scope.
function _xfa_scope_(node) {
    if (!node || (!node.parent && node !== xfa.form)) node = _xfa_context_node_() || xfa.form;
    if (!Object.prototype.hasOwnProperty.call(node, '_xfa_scope_proxy_')) {
        _xfa_hidden_(node, '_xfa_scope_proxy_', new Proxy({}, {
            has: function(t, k) { return _xfa_scope_value_(node, k) !== undefined; },
            get: function(t, k) { return _xfa_scope_value_(node, k); },
            set: function(t, k, v) {
                if (_xfa_own_scope_props_[k] === true) { node[k] = v; return true; }
                throw new TypeError("assignment to the form node '" + String(k) + "'");
            }
        }));
    }
    return node._xfa_scope_proxy_;
}

function _xfa_select_(list, index) {
    if (index === '*') return list.slice();
    return (index < list.length && list[index]) ? [list[index]] : [];
}

// `Name`, `Name[3]`, `Name[*]` -> {name, index}; `#class` -> {cls}; null
// for anything else.
function _xfa_parse_segment_(seg) {
    var c = /^#([A-Za-z]+)$/.exec(seg);
    if (c) return { cls: c[1] };
    var m = /^([^\[\]#.]+)(?:\[\s*(\*|\d+)\s*\])?$/.exec(seg);
    if (!m) return null;
    return {
        name: m[1],
        indexed: m[2] !== undefined,
        index: (m[2] === undefined) ? 0 : (m[2] === '*' ? '*' : parseInt(m[2], 10))
    };
}

// A class reference (XFA 3.3 §3, `#class`): the children of `obj` of class
// `cls`. Only property subtrees carry a class here (a caption's `value`
// holds a `text` whose `className` is "text"); a form node's children are
// named, not classed, so `#field` and `#subform` are not resolved.
function _xfa_kids_of_class_(obj, cls) {
    if (obj === null || typeof obj !== 'object') return [];
    var out = [];
    var keys = Object.keys(obj);
    for (var i = 0; i < keys.length; i++) {
        var v = obj[keys[i]];
        if (v !== null && typeof v === 'object' && v.className === cls) out.push(v);
    }
    return out;
}

// The first descendant of `root` named `name`, in document order (XFA 3.3
// §3 "Selecting Descendants At Any Level"), or null.
function _xfa_first_descendant_(root, name) {
    // `xfa..X` searches the document: of the models, only the Form DOM
    // holds nodes here.
    if (root === xfa) return _xfa_first_descendant_(xfa.form, name);
    if (root === null || typeof root !== 'object' || !root._xfa_kids_) return null;
    var kids = root._xfa_kids_;
    for (var key in kids) {
        var list = kids[key];
        for (var i = 0; i < list.length; i++) {
            var kid = list[i];
            if (!kid) continue;
            if (key === name) return kid;
            var below = _xfa_first_descendant_(kid, name);
            if (below) return below;
        }
    }
    return null;
}

// The object a SOM expression's first segment names when it is a model or
// a model shortcut (XFA 3.3 §3 "Shortcuts"), undefined when it is not one.
// Only the models this engine has are answered.
function _xfa_model_(name) {
    if (name === 'xfa' || name === '$xfa') return xfa;
    if (name === 'form' || name === '$form') return xfa.form;
    if (name === 'host' || name === '$host') return xfa.host;
    if (name === 'layout' || name === '$layout') return xfa.layout;
    if (name === 'event' || name === '$event') return xfa.event;
    return undefined;
}

// Every node a SOM expression names, resolved from `start` (the script's
// container; null for no context) by XFA 3.3 §3:
// - `$` and `this` are the container itself (see acrobat_compat.rs for
//   `this`); `$.X` is a child of the container, not a scope match;
// - an unqualified first segment is scope-matched (`_xfa_scope_walk_`),
//   or is a model (`xfa`, `form`); with no container it is a child of the
//   form root;
// - `A..B` is the first descendant of A named B in document order, and an
//   index then counts among that node's same-named siblings;
// - `#class` selects the children of that class;
// - a model this engine does not have (`$data`, `$record`, `!`) is an
//   error naming it, not a silent miss.
// Returns null for an expression that is not well formed.
function _xfa_resolve_nodes_(start, expr) {
    expr = String(expr).replace(/^\s+|\s+$/g, '');
    if (expr === '') return null;
    if (expr === '$' || expr === 'this') return start ? [start] : [];

    var raw = expr.split('.');
    var segs = [];
    var descend = false;
    for (var i = 0; i < raw.length; i++) {
        if (raw[i] === '') { descend = true; continue; }
        var seg = _xfa_parse_segment_(raw[i]);
        if (!seg) return null;
        seg.descend = descend;
        descend = false;
        segs.push(seg);
    }
    if (segs.length === 0 || descend) return null;

    var first = segs[0];
    var selected;
    var from = 0;
    if (first.cls !== undefined) return null;
    if (!first.descend && (first.name === '$' || first.name === 'this')) {
        selected = start ? [start] : [];
        from = 1;
    } else if (first.name.charAt(0) === '$' || first.name.charAt(0) === '!') {
        var model = _xfa_model_(first.name);
        if (model === undefined) {
            throw new Error("SOM expression '" + expr + "' starts at " + first.name
                + ", a model this engine does not have");
        }
        selected = [model];
        from = 1;
    } else {
        selected = start ? [start] : [xfa.form];
        if (!first.descend) {
            var found = start
                ? _xfa_scope_walk_(start, first.name, first.indexed)
                : _xfa_kids_of_(xfa.form, first.name);
            if (found.length === 0) {
                var m = _xfa_model_(first.name);
                found = m === undefined ? [] : [m];
                selected = _xfa_select_(found, 0);
            } else {
                selected = _xfa_select_(found, first.index);
            }
            from = 1;
        }
    }

    for (var s = from; s < segs.length; s++) {
        var seg = segs[s];
        var next = [];
        for (var k = 0; k < selected.length; k++) {
            if (seg.cls !== undefined) {
                next = next.concat(_xfa_kids_of_class_(selected[k], seg.cls));
            } else if (seg.descend) {
                var hit = _xfa_first_descendant_(selected[k], seg.name);
                if (hit) {
                    next = next.concat(_xfa_select_(_xfa_kids_of_(hit.parent, seg.name), seg.index));
                }
            } else {
                next = next.concat(_xfa_select_(_xfa_kids_of_(selected[k], seg.name), seg.index));
            }
        }
        selected = next;
    }
    return selected;
}

function _xfa_resolve_node_from_(node, expr) {
    var found = _xfa_resolve_nodes_(node, expr);
    return (found && found.length > 0) ? found[0] : null;
}

function _xfa_resolve_nodes_from_(node, expr) {
    return _xfa_collection_(_xfa_resolve_nodes_(node, expr) || []);
}

// The node `xfa.resolveNode` resolves relative to: the current script's own
// container -- the nearest registered node on its path, since a script can
// run with a context that names a node with no object of its own -- or the
// form root (`null`) when no script is running.
function _xfa_context_node_() {
    // The running script's own container, when it is a linked node: the
    // registry by path is not filled in every engine (the load pass's).
    if (typeof _xfa_this_ !== 'undefined' && _xfa_this_ !== null
        && typeof _xfa_this_ === 'object'
        && Object.prototype.hasOwnProperty.call(_xfa_this_, 'parent')) {
        return _xfa_this_;
    }
    var path = _xfa_current_context_ ? String(_xfa_current_context_) : '';
    while (path) {
        if (_xfa_fields_by_path_[path]) return _xfa_fields_by_path_[path];
        var dot = path.lastIndexOf('.');
        path = dot >= 0 ? path.substring(0, dot) : '';
    }
    return null;
}

xfa.resolveNode = function(expr) { return _xfa_resolve_node_from_(_xfa_context_node_(), expr); };
xfa.resolveNodes = function(expr) { return _xfa_resolve_nodes_from_(_xfa_context_node_(), expr); };

// Node-level `resolveNode`/`resolveNodes` (XFA 3.3 §3): relative to the node
// they are called on.
function _xfa_node_resolveNode_(expr) { return _xfa_resolve_node_from_(this, expr); }
function _xfa_node_resolveNodes_(expr) { return _xfa_resolve_nodes_from_(this, expr); }

// `getDeltas()` (LiveCycle Designer Scripting Reference; not in XFA 3.3):
// one delta per field or exclusion group in this subtree whose value has
// left the value it was merged with, each with `target`, `currentValue`,
// `savedValue` and `restore()`. A blank form opens with none.
function _xfa_value_text_(v) {
    return (v === null || v === undefined) ? '' : String(v);
}

function _xfa_node_getDeltas_() {
    var out = [];
    var walk = function(n) {
        if (Object.prototype.hasOwnProperty.call(n, '_xfa_mergedValue_')) {
            var saved = n._xfa_mergedValue_;
            var current = n.rawValue;
            if (_xfa_value_text_(current) !== _xfa_value_text_(saved)) {
                out.push({
                    target: n,
                    currentValue: current,
                    savedValue: saved,
                    restore: function() { n.rawValue = saved; }
                });
            }
        }
        if (!Object.prototype.hasOwnProperty.call(n, '_xfa_kids_')) return;
        for (var name in n._xfa_kids_) {
            var kids = n._xfa_kids_[name];
            for (var i = 0; i < kids.length; i++) if (kids[i]) walk(kids[i]);
        }
    };
    walk(this);
    return _xfa_collection_(out);
}

// The names a form node answers even when this engine does not model them:
// XFA 3.3's container properties and the scripting properties and methods
// of field, subform, draw and exclusion group objects. Reading one of these
// that a node lacks gives `undefined`, as it always has.
var _xfa_node_property_names_ = {};
('access accessKey anchorType colSpan columnWidths h hAlign id layout locale maxH maxW '
 + 'minH minW name presence relevant restoreState rotate scope use usehref vAlign w x y '
 + 'allowMacro mergeMode assist bind bookend border break breakAfter breakBefore calculate '
 + 'caption connect desc event extras field font format items keep margin occur overflow '
 + 'pageSet para proto setProperty subform subformSet traversal ui validate value variables '
 + 'draw exclGroup area exObject all borderColor borderWidth className classAll classIndex '
 + 'dataNode editValue errorText fillColor fontColor formattedValue index instanceIndex '
 + 'instanceManager isContainer isNull length mandatory mandatoryMessage model ns nodes '
 + 'oneOfChild parent parentSubform rawValue selectedIndex somExpression validationMessage '
 + 'textEntry addItem applyXSL assignNode boundItem clearErrorList clearItems clone '
 + 'createNode deleteItem execCalculate execEvent execInitialize execValidate getAttribute '
 + 'getDelta getDeltas getDisplayItem getElement getItemState getSaveItem '
 + 'isPropertySpecified loadXML resolveNode resolveNodes saveFilteredXML saveXML '
 + 'setAttribute setElement setFocus setItemState setItems addInstance removeInstance '
 + 'moveInstance insertInstance setInstances count min max').split(' ').forEach(function(n) {
    _xfa_node_property_names_[n] = true;
});

// The prototype of every form node object.
//
// A name that is not a property of the object is a SOM reference resolved
// from it. XFA 3.3 §3 calls a JavaScript `this.X` qualified, which would
// confine `X` to the object's children; Acrobat resolves it with the same
// scope walk as an unqualified name (children, then the container and its
// siblings, then each ancestor's), and forms depend on that: ACYI_001_EN's
// row-adding button does `this.GWM_Option_DYN` for the row it sits in, and
// a field's own calculate does `this.Branch_Flag.rawValue` for itself.
//
// Acrobat throws when nothing in scope has the name, and forms rely on
// that too: AALP_019_EN tries `xfa.form.UBSForms.UBSForms_<id>` inside
// `try` to fall back to the root. This is a documented deviation from the
// specification: see xfa/acrobat_compat.rs. Names on `Object.prototype`, symbols,
// engine-internal `_` names and the XFA names above are answered normally.
var _xfa_node_proto_ = new Proxy(Object.create(Object.prototype), {
    get: function(target, key, receiver) {
        if (typeof key !== 'string' || key in Object.prototype) {
            return Reflect.get(target, key, receiver);
        }
        if (key === 'nodes') return _xfa_nodes_of_(receiver);
        if (key.charAt(0) === '_' || key.charAt(0) === '$'
            || _xfa_node_property_names_[key] === true) {
            return undefined;
        }
        var found = _xfa_scope_walk_(receiver, key);
        if (found.length > 0) return found[0];
        var where = Object.prototype.hasOwnProperty.call(receiver, 'somExpression')
            ? String(receiver.somExpression) : 'this object';
        throw new Error("Invalid property get operation; " + where
            + " doesn't have property '" + key + "'");
    }
});

// A field's caption as a property subtree a script can reach by SOM
// expression (XFA 3.3 §3 class references, `#class`): `caption.value.#text` is the
// `<text>` under the caption's `<value>`, and assigning its `value` relabels
// the field, the way ACYI_001_EN's buttons take their language-specific
// label. Only a caption whose template holds a `<text>` gets one -- a
// rich-text (`exData`) caption has no `#text`, in Acrobat either.
function _xfa_init_caption_(node, text) {
    if (text === null || text === undefined) return;
    var caption = node.caption;
    if (caption === null || typeof caption !== 'object') return;
    var value = caption.value;
    if (value === null || typeof value !== 'object') {
        value = {};
        caption.value = value;
    }
    value.text = { className: 'text', value: String(text) };
    _xfa_hidden_(caption, '_xfa_initialText_', String(text));
}

// The caption text of `node` if a script changed it since the last call,
// else null; the baseline moves to the new text.
function _xfa_take_caption_change_(node) {
    var caption = node.caption;
    if (caption === null || typeof caption !== 'object'
        || !Object.prototype.hasOwnProperty.call(caption, '_xfa_initialText_')) return null;
    var value = caption.value;
    if (value === null || typeof value !== 'object') return null;
    var text = value.text;
    if (text === null || typeof text !== 'object') return null;
    var now = _xfa_value_text_(text.value);
    if (now === caption._xfa_initialText_) return null;
    caption._xfa_initialText_ = now;
    return now;
}

// A field's choice list as one string, for telling a script's
// `addItem`/`clearItems` apart from the list the field was registered with.
function _xfa_items_key_(node) {
    var items = node._items || [];
    var out = [];
    for (var i = 0; i < items.length; i++) out.push([String(items[i].d), String(items[i].s)]);
    return JSON.stringify(out);
}

// The field's items if a script changed them since the last call, as the
// JSON `_xfa_items_key_` builds, else null; the baseline moves to them.
function _xfa_take_items_change_(node) {
    if (!Object.prototype.hasOwnProperty.call(node, '_xfa_initialItems_')) return null;
    var now = _xfa_items_key_(node);
    if (now === node._xfa_initialItems_) return null;
    node._xfa_initialItems_ = now;
    return now;
}

// Put `node`'s `access` and caption text back to their baselines (what
// `take_access_changes` / `take_caption_changes` compare against): the
// state a master-page node has outside any one page's evaluation.
function _xfa_reset_to_baseline_(node) {
    if (node._initialAccess !== undefined) node.access = node._initialAccess;
    var caption = node.caption;
    if (caption === null || typeof caption !== 'object'
        || !Object.prototype.hasOwnProperty.call(caption, '_xfa_initialText_')) return;
    var value = caption.value;
    if (value === null || typeof value !== 'object') return;
    var text = value.text;
    if (text === null || typeof text !== 'object') return;
    text.value = caption._xfa_initialText_;
}

function _xfa_install_node_methods_(obj) {
    Object.setPrototypeOf(obj, _xfa_node_proto_);
    _xfa_hidden_(obj, 'resolveNode', _xfa_node_resolveNode_);
    _xfa_hidden_(obj, 'resolveNodes', _xfa_node_resolveNodes_);
    _xfa_hidden_(obj, 'getDeltas', _xfa_node_getDeltas_);
}
"#;

/// The XFA 3.3 §9 instance manager.
///
/// One `_XfaInstanceManager` per array of same-named sibling subforms, stored
/// on their parent as `_Name`. Its `_instances` is the very array the parent's
/// child links keep, so registering an instance adds it here too.
///
/// A method within `[min, max]` changes the array at once, so the script that
/// called it can address the new instance in the same run (the corpus
/// `soPlusMinus.applyIndex` numbers every row right after `addInstance`):
/// a new instance is a *placeholder*, a copy of instance 0's object shape with
/// every value emptied. It also queues the change in `_xfa_instance_ops`,
/// which the engine drains after each script and applies to the Form DOM,
/// replacing the placeholder with an instance built from the template; values
/// the script wrote into the placeholder are carried over, anything it read
/// from it was empty. A method outside `[min, max]` does nothing and returns
/// `null`, as in Acrobat, and queues a `limit` record so the host can say why
/// nothing happened.
pub const XFA_INSTANCE_MANAGER: &str = r#"
var _xfa_instance_ops = [];

function _xfa_drain_instance_ops_() {
    var ops = _xfa_instance_ops;
    _xfa_instance_ops = [];
    return ops;
}

function _XfaInstanceManager(parent, parentPath, name, min, max) {
    _xfa_hidden_(this, '_parentObj', parent);
    _xfa_hidden_(this, '_parentPath', parentPath);
    _xfa_hidden_(this, '_childName', name);
    _xfa_hidden_(this, '_instances', _xfa_kid_list_(parent, name));
    this.name = '_' + name;
    this.min = min;
    this.max = max;
}

Object.defineProperty(_XfaInstanceManager.prototype, 'count', {
    get: function() { return this._instances.length; },
    enumerable: true, configurable: true
});

// `reason`: 'max' (adding past max), 'min' (removing below min), 'clamp'
// (setInstances outside [min, max]), 'index' (no instance at that index).
_XfaInstanceManager.prototype._limit = function(method, reason, bound) {
    _xfa_instance_ops.push({
        op: 'limit', parent: this._parentPath, name: this._childName,
        method: method, reason: reason, count: this.count, bound: bound
    });
    return null;
};

_XfaInstanceManager.prototype._renumber = function() {
    var parent = this._parentObj;
    for (var i = 0; i < this._instances.length; i++) {
        this._instances[i].index = i;
        this._instances[i].instanceIndex = i;
    }
    if (this._instances.length > 0) {
        _xfa_set_child_(parent, this._childName, this._instances[0]);
    } else {
        delete parent[this._childName];
    }
};

_XfaInstanceManager.prototype._full = function() {
    return this.max !== -1 && this.count >= this.max;
};

_XfaInstanceManager.prototype.insertInstance = function(index, bMerge) {
    index = Number(index);
    if (!(index >= 0 && index <= this.count) || index !== Math.floor(index)) {
        return this._limit('insertInstance', 'index', this.count);
    }
    if (this._full()) return this._limit(index === this.count ? 'addInstance' : 'insertInstance', 'max', this.max);
    var placeholder = _xfa_placeholder_(this, this._parentObj);
    this._instances.splice(index, 0, placeholder);
    this._renumber();
    _xfa_instance_ops.push({
        op: 'insert', parent: this._parentPath, name: this._childName, index: index
    });
    return placeholder;
};

_XfaInstanceManager.prototype.addInstance = function(bMerge) {
    return this.insertInstance(this.count, bMerge);
};

_XfaInstanceManager.prototype.removeInstance = function(index) {
    index = Number(index);
    if (!(index >= 0 && index < this.count) || index !== Math.floor(index)) {
        return this._limit('removeInstance', 'index', this.count - 1);
    }
    if (this.count <= this.min) return this._limit('removeInstance', 'min', this.min);
    this._instances.splice(index, 1);
    this._renumber();
    _xfa_instance_ops.push({
        op: 'remove', parent: this._parentPath, name: this._childName, index: index
    });
    return null;
};

_XfaInstanceManager.prototype.moveInstance = function(from, to) {
    from = Number(from);
    to = Number(to);
    var n = this.count;
    if (!(from >= 0 && from < n && to >= 0 && to < n)) {
        return this._limit('moveInstance', 'index', n - 1);
    }
    if (from === to) return null;
    var moved = this._instances.splice(from, 1)[0];
    this._instances.splice(to, 0, moved);
    this._renumber();
    _xfa_instance_ops.push({
        op: 'move', parent: this._parentPath, name: this._childName, from: from, to: to
    });
    return null;
};

_XfaInstanceManager.prototype.setInstances = function(n) {
    n = Math.floor(Number(n));
    var target = Math.max(n, this.min);
    if (this.max !== -1) target = Math.min(target, this.max);
    if (target !== n) this._limit('setInstances', 'clamp', target);
    while (this.count < target) this.addInstance();
    while (this.count > target) this.removeInstance(this.count - 1);
    return null;
};

// The manager of `name` under `parent`, created with limits `min`/`max`
// when there is none yet.
function _xfa_manager_(parent, name, min, max) {
    var key = '_' + name;
    if (!Object.prototype.hasOwnProperty.call(parent, key)) {
        var parentPath = parent.somExpression ? String(parent.somExpression) : '';
        _xfa_hidden_(parent, key, new _XfaInstanceManager(parent, parentPath, name, min, max));
    }
    return parent[key];
}

// Install the declared limits; the engine calls this for every repeatable
// subform, including one with no instance at all (initial 0), so
// `_Row.addInstance()` works from zero.
function _xfa_install_manager_(parent, parentPath, name, min, max) {
    var manager = _xfa_manager_(parent, name, min, max);
    manager.min = min;
    manager.max = max;
    manager._parentPath = parentPath;
    return manager;
}

// A copy of `source`'s object shape: the same child objects (copied, not
// shared) and the same accessors and methods, every value emptied. Child
// links, exclusion-group links and nested managers are rebuilt for the copy.
function _xfa_shape_copy_(source, parent, name, index) {
    var copy = {};
    var kids = source._xfa_kids_ || {};
    var keys = Object.getOwnPropertyNames(source);
    for (var k = 0; k < keys.length; k++) {
        var key = keys[k];
        if (key === 'parent' || key === '_xfa_kids_' || key === 'instanceManager'
            || key === '_exclGroupParent' || kids[key] !== undefined
            || (key.charAt(0) === '_' && kids[key.substring(1)] !== undefined)) {
            continue;
        }
        var desc = Object.getOwnPropertyDescriptor(source, key);
        if (key === '_rawValue') desc.value = '';
        if (key === '_items' && desc.value) desc.value = desc.value.slice();
        Object.defineProperty(copy, key, desc);
    }
    _xfa_link_child_(parent, name, index, copy, !!source.instanceManager);
    for (var kidName in kids) {
        var list = kids[kidName];
        var nested = source['_' + kidName];
        if (nested instanceof _XfaInstanceManager) {
            var manager = _xfa_manager_(copy, kidName, nested.min, nested.max);
            manager.min = nested.min;
            manager.max = nested.max;
        }
        for (var i = 0; i < list.length; i++) {
            if (!list[i]) continue;
            var kid = _xfa_shape_copy_(list[i], copy, kidName, i);
            if (list[i]._exclGroupParent === source) {
                _xfa_hidden_(kid, '_exclGroupParent', copy);
            }
        }
    }
    _xfa_hidden_(copy, '_xfa_placeholder_', true);
    return copy;
}

// The instances of `name` under `parent` as the scripts left them, in order:
// each `{ path, placeholder, values }`, where `path` is the path the engine
// registered a real instance under, and `values` (placeholders only) maps
// each descendant's path relative to the instance to the non-empty value a
// script wrote into it.
function _xfa_instance_state_(parent, name) {
    var list = _xfa_kid_list_(parent, name);
    var out = [];
    for (var i = 0; i < list.length; i++) {
        var inst = list[i];
        if (!inst) continue;
        var placeholder = !!inst._xfa_placeholder_;
        var values = {};
        if (placeholder) _xfa_harvest_(inst, '', values);
        out.push({
            path: placeholder ? '' : String(inst.somExpression),
            placeholder: placeholder,
            values: values
        });
    }
    return out;
}

function _xfa_harvest_(node, prefix, out) {
    if (prefix && node._rawValue !== undefined && node._rawValue !== null
        && String(node._rawValue) !== '') {
        out[prefix] = String(node._rawValue);
    }
    var kids = node._xfa_kids_ || {};
    for (var kidName in kids) {
        var list = kids[kidName];
        for (var i = 0; i < list.length; i++) {
            if (!list[i]) continue;
            var seg = i === 0 ? kidName : kidName + '[' + i + ']';
            _xfa_harvest_(list[i], prefix ? prefix + '.' + seg : seg, out);
        }
    }
}

// A new instance for `manager`, standing in until the engine builds the real
// one: a shape copy of instance 0, or a bare node when there is none.
function _xfa_placeholder_(manager, parent) {
    var name = manager._childName;
    var list = manager._instances;
    var index = list.length;
    var placeholder;
    if (list.length > 0) {
        placeholder = _xfa_shape_copy_(list[0], parent, name, index);
        // `_xfa_shape_copy_` linked it at the end; the caller places it.
        list.pop();
    } else {
        placeholder = { name: name };
        _xfa_hidden_(placeholder, 'parent', parent);
        _xfa_hidden_(placeholder, '_xfa_placeholder_', true);
        placeholder.instanceManager = manager;
        _xfa_install_node_methods_(placeholder);
    }
    return placeholder;
}
"#;

/// Combined JavaScript helpers for XFA environment setup.
pub fn get_all_helpers() -> String {
    format!(
        "{XFA_SOM_WALKER}\n{XFA_INSTANCE_MANAGER}\n{XFA_HOST_EXTENSIONS}\n{XFA_VALUE_FORMAT}\n{XFA_SCRIPT_OBJECTS}\n{XFA_FORMCALC_RUNTIME}"
    )
}

/// The host methods forms call that XFA 3.3 leaves to the application: the
/// Acrobat globals `app` and `util`, and `xfa.host.messageBox`, `resetData`,
/// `print` and `numPages` (see [`super::acrobat`] for the references).
///
/// A dialog has nobody to answer it, so it takes its default button (OK, or
/// Yes) and is queued in `_xfa_host_messages` with that answer, for the host
/// to report: what a form says to the person filling it is part of what it
/// did. `util.printd` formats in Rust (`_xfa_printd_native_`).
///
/// `resetData` resets each field to its template default (the value in the
/// template before any script ran, `_xfa_template_defaults_`, keyed by the
/// field's path without instance indices so a new instance resets too),
/// at once, so a script that resets a section and then fills it keeps its
/// own writes. It takes a comma-separated list of SOM expressions, each
/// naming a field or a container whose fields all reset; no list resets the
/// whole form. An expression that names nothing is skipped, as Acrobat does,
/// and reported through `_xfa_host_errors`.
pub const XFA_HOST_EXTENSIONS: &str = r#"
var _xfa_host_messages = [];
var _xfa_host_errors = [];
var _xfa_template_defaults_ = {};

function _xfa_drain_host_messages_() {
    var m = _xfa_host_messages;
    _xfa_host_messages = [];
    return m;
}

function _xfa_drain_host_errors_() {
    var e = _xfa_host_errors;
    _xfa_host_errors = [];
    return e;
}

function _xfa_dialog_(source, message, title, type) {
    var answer = _xfa_dialog_answer_native_(type === undefined || type === null ? 0 : Number(type));
    _xfa_host_messages.push({
        source: source,
        text: String(message),
        title: title === undefined || title === null ? '' : String(title),
        answer: answer
    });
    return answer;
}

var app = {
    alert: function(cMsg, nIcon, nType, cTitle) {
        if (cMsg !== null && typeof cMsg === 'object') {
            nType = cMsg.nType;
            cTitle = cMsg.cTitle;
            cMsg = cMsg.cMsg;
        }
        return _xfa_dialog_('app.alert', cMsg, cTitle, nType);
    },
    beep: function() {}
};

var util = {
    printd: function(cFormat, oDate) {
        var d = oDate instanceof Date ? oDate : new Date(oDate);
        return _xfa_printd_native_(
            typeof cFormat === 'number' ? cFormat : String(cFormat),
            d.getFullYear(), d.getMonth() + 1, d.getDate(),
            d.getHours(), d.getMinutes(), d.getSeconds(), d.getDay()
        );
    }
};

xfa.host.messageBox = function(cMessage, cTitle, nIcon, nType) {
    return _xfa_dialog_('xfa.host.messageBox', cMessage, cTitle, nType);
};

xfa.host.print = function() {};

Object.defineProperty(xfa.host, 'numPages', {
    get: function() { return xfa.layout.pageCount(); },
    enumerable: true, configurable: true
});

function _xfa_index_free_(path) {
    return String(path).replace(/\[[^\]]*\]/g, '');
}

function _xfa_is_under_(path, prefix) {
    if (path === prefix) return true;
    if (path.indexOf(prefix) !== 0) return false;
    var next = path.charAt(prefix.length);
    return next === '.' || next === '[';
}

xfa.host.resetData = function(somList) {
    var prefixes = null;
    if (somList !== undefined && somList !== null && String(somList) !== '') {
        prefixes = [];
        var exprs = String(somList).split(',');
        for (var i = 0; i < exprs.length; i++) {
            var expr = exprs[i].replace(/^\s+|\s+$/g, '');
            if (!expr) continue;
            var node = xfa.resolveNode(expr);
            if (!node || !node.somExpression) {
                _xfa_host_errors.push('xfa.host.resetData: ' + expr + ' names no node; skipped');
                continue;
            }
            prefixes.push(String(node.somExpression));
        }
    }
    for (var path in _xfa_fields_by_path_) {
        var obj = _xfa_fields_by_path_[path];
        if (!obj || String(obj.somExpression) !== path) continue;
        var key = _xfa_index_free_(path);
        if (!Object.prototype.hasOwnProperty.call(_xfa_template_defaults_, key)) continue;
        if (prefixes !== null) {
            var hit = false;
            for (var j = 0; j < prefixes.length && !hit; j++) hit = _xfa_is_under_(path, prefixes[j]);
            if (!hit) continue;
        }
        obj.rawValue = _xfa_template_defaults_[key];
    }
};
"#;

/// A field's `formattedValue` and `editValue` (XFA 3.3 "Scripting Object
/// Model"; the LiveCycle field properties): its `rawValue` through its
/// format picture or its edit picture, and, when assigned, the text read
/// back through the same picture into `rawValue`. A field with no such
/// picture, or a value the picture does not fit, reads as its raw value.
///
/// The pictures and the locale's JSON are kept on the field object
/// (`_xfa_install_value_format_`, called by the host for every field);
/// `_xfa_picture_native_` does the formatting in Rust.
pub const XFA_VALUE_FORMAT: &str = r#"
var _xfa_locales_ = {};

function _xfa_through_picture_(mode, obj, picture, text) {
    if (!picture) return null;
    return _xfa_picture_native_(mode, picture, text, _xfa_locales_[obj._xfa_locale_] || '');
}

function _xfa_shown_(obj, picture) {
    var raw = obj.rawValue;
    if (raw === null || raw === undefined) return '';
    raw = String(raw);
    var shown = _xfa_through_picture_('format', obj, picture, raw);
    return shown === null ? raw : shown;
}

function _xfa_read_back_(obj, picture, text) {
    text = text === null || text === undefined ? '' : String(text);
    var raw = _xfa_through_picture_('parse', obj, picture, text);
    obj.rawValue = raw === null ? text : raw;
}

function _xfa_install_value_format_(obj, format, edit, localeName) {
    _xfa_hidden_(obj, '_xfa_format_', format);
    _xfa_hidden_(obj, '_xfa_edit_', edit);
    _xfa_hidden_(obj, '_xfa_locale_', localeName);
    Object.defineProperty(obj, 'formattedValue', {
        get: function() { return _xfa_shown_(this, this._xfa_format_); },
        set: function(text) { _xfa_read_back_(this, this._xfa_format_, text); },
        enumerable: false, configurable: true
    });
    Object.defineProperty(obj, 'editValue', {
        get: function() { return _xfa_shown_(this, this._xfa_edit_); },
        set: function(text) { _xfa_read_back_(this, this._xfa_edit_, text); },
        enumerable: false, configurable: true
    });
}
"#;

/// Named script objects (`<variables><script name>`). XFA 3.3 §10
/// "Instantiation of Named Script Objects": the variables of a subform are
/// instantiated with the subform, and "when the document variable is
/// referenced the compiled script object is returned". So every object is
/// declared first, then instantiated in document order
/// (`_xfa_instantiate_script_objects_`), and a reference to one not yet
/// built builds it on the spot: a body may use an object declared after it
/// (`try { myDE; } catch (e) { soCommonLabelDefinition; }` at the top of an
/// earlier object), and the globals a body assigns exist for every script
/// that runs afterwards.
///
/// Each declaration is an accessor on its owner subform ("registered with
/// the subform"); the first declaration of a name is also the global of
/// that name. The factory runs in its owner's scope, so the object's
/// functions resolve unqualified names from the subform that declares it.
/// A factory that throws fails every reference the same way; one that
/// refers to itself while being built is an error.
pub const XFA_SCRIPT_OBJECTS: &str = r#"
var _xfa_script_objects_ = {};

function _xfa_script_object_value_(entry) {
    if (entry.state === 'done') return entry.value;
    if (entry.state === 'error') throw entry.error;
    if (entry.state === 'building') {
        throw new Error("script object '" + entry.name + "' refers to itself while it is instantiated");
    }
    entry.state = 'building';
    try {
        var build = (0, eval)(
            '(function(_xfa_scope_proxy_) { with (_xfa_scope_proxy_) { return ' + entry.factory + '(); } })'
        );
        entry.value = build(_xfa_scope_(entry.owner));
        entry.state = 'done';
        return entry.value;
    } catch (e) {
        entry.state = 'error';
        entry.error = e;
        throw e;
    }
}

function _xfa_instantiate_script_objects_() {
    for (var name in _xfa_script_objects_) {
        var list = _xfa_script_objects_[name];
        for (var i = 0; i < list.length; i++) {
            if (list[i].state !== 'pending') continue;
            try {
                _xfa_script_object_value_(list[i]);
            } catch (e) {
                _xfa_host_errors.push("script object '" + name + "' failed to instantiate: " + e);
            }
        }
    }
}

function _xfa_define_script_object_(name, owner, factory) {
    var entry = { name: name, owner: owner, factory: factory, state: 'pending' };
    var list = _xfa_script_objects_[name] || (_xfa_script_objects_[name] = []);
    list.push(entry);
    var accessor = {
        get: function() { return _xfa_script_object_value_(entry); },
        set: function(v) { entry.state = 'done'; entry.value = v; },
        enumerable: false,
        configurable: true
    };
    if (list.length === 1) Object.defineProperty(_xfa_global_, name, accessor);
    if (owner) Object.defineProperty(owner, name, accessor);
}
"#;

/// The runtime of FormCalc translated to JavaScript (see `formcalc.rs`;
/// XFA 3.3 Chapter 25). FormCalc is value-oriented: an operand that names a
/// node is the node's value. Arithmetic on two nulls is null, a non-numeric
/// operand counts as 0, comparisons and logical operators give 1 or 0, and
/// division by zero is an error, as the spec's "Basic Expressions" say.
pub const XFA_FORMCALC_RUNTIME: &str = r#"
function _fc_is_node_(x) {
    return x !== null && typeof x === 'object'
        && Object.prototype.hasOwnProperty.call(x, 'somExpression');
}

function _fc_v_(x) {
    if (_fc_is_node_(x)) return (x.rawValue === undefined) ? null : x.rawValue;
    return (x === undefined) ? null : x;
}

function _fc_num_(v) {
    if (v === null || v === '') return 0;
    var n = Number(v);
    return isNaN(n) ? 0 : n;
}

function _fc_numeric_(v) {
    return v !== null && v !== '' && !isNaN(Number(v));
}

function _fc_b_(v) { return _fc_num_(_fc_v_(v)) !== 0; }

function _fc_get_(base, name) {
    if (base === null || base === undefined) {
        throw new Error("FormCalc: '." + name + "' of nothing");
    }
    var kids = _xfa_kids_of_(base, name);
    if (kids.length > 0) return kids[0];
    if (name in Object(base)) return base[name];
    throw new Error("FormCalc: " + (base.somExpression || "the object") + " has no '" + name + "'");
}

function _fc_index_(base, name, i) {
    var found = _xfa_select_(_xfa_kids_of_(base, name), i);
    if (found.length === 0) throw new Error("FormCalc: no " + name + "[" + i + "]");
    return found[0];
}

function _fc_first_(node, name, i) {
    var found = _xfa_select_(_xfa_scope_walk_(node, name, true), i);
    if (found.length === 0) throw new Error("FormCalc: no " + name + "[" + i + "]");
    return found[0];
}

// A method call on an object (XFA 3.3 §25 "Method Calls"): names are
// case-insensitive, so `$host.MessageBox` finds `messageBox`; an object
// without the method is an error, as calling nothing is.
function _fc_call_(base, name, args) {
    if (base === null || base === undefined) {
        throw new Error("FormCalc: '." + name + "()' of nothing");
    }
    var target = Object(base);
    // Found by name along the prototype chain, never by a plain read: a
    // form node throws for a name it does not have (as Acrobat does).
    var lower = String(name).toLowerCase();
    var method;
    for (var obj = target; obj && typeof method !== 'function'; obj = Object.getPrototypeOf(obj)) {
        var names = Object.getOwnPropertyNames(obj);
        var found = names.indexOf(String(name));
        if (found < 0) {
            for (var i = 0; i < names.length; i++) {
                if (names[i].toLowerCase() === lower) { found = i; break; }
            }
        }
        if (found >= 0 && typeof target[names[found]] === 'function') method = target[names[found]];
    }
    if (typeof method !== 'function') {
        throw new Error("FormCalc: " + (base.somExpression || "the object") + " has no method '" + name + "'");
    }
    return method.apply(target, args);
}

function _fc_set_node_(target, v) {
    if (!_fc_is_node_(target)) throw new Error("FormCalc: assignment to a value, not a node");
    target.rawValue = v;
    return v;
}

function _fc_set_(base, name, v) {
    var kids = _xfa_kids_of_(base, name);
    if (kids.length > 0) {
        kids[0].rawValue = v;
    } else if (name in Object(base)) {
        base[name] = v;
    } else {
        throw new Error("FormCalc: " + (base.somExpression || "the object") + " has no '" + name + "'");
    }
    return v;
}

function _fc_cmp_(op, a, b) {
    var c;
    if ((op === '==' || op === '<>') && (a === null || b === null)) {
        c = (a === null && b === null) ? 0 : 1;
    } else if (_fc_numeric_(a) || _fc_numeric_(b) || a === null || b === null) {
        var x = _fc_num_(a), y = _fc_num_(b);
        c = x < y ? -1 : (x > y ? 1 : 0);
    } else {
        var s = String(a), t = String(b);
        c = s < t ? -1 : (s > t ? 1 : 0);
    }
    switch (op) {
        case '==': return c === 0;
        case '<>': return c !== 0;
        case '<': return c < 0;
        case '<=': return c <= 0;
        case '>': return c > 0;
        default: return c >= 0;
    }
}

function _fc_op_(op, a, b) {
    a = _fc_v_(a);
    b = _fc_v_(b);
    switch (op) {
        case '+': case '-': case '*': case '/':
            if (a === null && b === null) return null;
            var x = _fc_num_(a), y = _fc_num_(b);
            if (op === '+') return x + y;
            if (op === '-') return x - y;
            if (op === '*') return x * y;
            if (y === 0) throw new Error("FormCalc: division by zero");
            return x / y;
        case 'and': return (_fc_b_(a) && _fc_b_(b)) ? 1 : 0;
        case 'or': return (_fc_b_(a) || _fc_b_(b)) ? 1 : 0;
        default: return _fc_cmp_(op, a, b) ? 1 : 0;
    }
}

function _fc_unary_(op, a) {
    a = _fc_v_(a);
    if (op === 'not') return _fc_b_(a) ? 0 : 1;
    if (a === null) return null;
    var x = _fc_num_(a);
    return op === '-' ? -x : x;
}
"#;
