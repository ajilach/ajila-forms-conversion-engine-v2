//! JavaScript Helper Code
//!
//! This module contains JavaScript helper code that is injected into the
//! scripting environment to provide XFA-specific functionality.

/// Global SOM resolution helper function.
///
/// When a path like "Page.SectionTitle.STP_SectionTitle.ffrb1" is accessed,
/// JavaScript property chain works for subforms but fails for floating fields.
/// This helper provides fallback resolution.
pub const XFA_RESOLVE_PATH_HELPER: &str = r#"
function _xfa_resolve_path_(path) {
    var parts = path.split('.');
    var obj = this; // Start from global
    
    // Try to traverse the path
    for (var i = 0; i < parts.length; i++) {
        var part = parts[i];
        // `in` first: reading a name a form node does not have throws
        // (see `_xfa_node_proto_`).
        if (obj && (part in Object(obj)) && typeof obj[part] !== 'undefined') {
            obj = obj[part];
        } else {
            // Path traversal failed - try looking up the last part in the registry
            var lastPart = parts[parts.length - 1];
            if (typeof _xfa_fields_ !== 'undefined' && _xfa_fields_[lastPart]) {
                return _xfa_fields_[lastPart];
            }
            return null;
        }
    }
    return obj;
}
"#;

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
function _xfa_link_child_(parent, name, index, child, isSubform) {
    var list = _xfa_kid_list_(parent, name);
    list[index] = child;
    if (index === 0) {
        parent[name] = child;
    }
    _xfa_hidden_(child, 'parent', parent);
    child.index = index;
    if (isSubform) {
        var manager = _xfa_manager_(parent, name, 1, 1);
        child.instanceManager = manager;
        child.instanceIndex = index;
    }
}


// A node placed directly in a pageArea: the pageArea becomes its parent,
// and whatever it was linked under (the root subform) the pageArea's.
function _xfa_reparent_under_page_area_(pageArea, child) {
    if (!Object.prototype.hasOwnProperty.call(pageArea, 'parent') && child.parent) {
        _xfa_hidden_(pageArea, 'parent', child.parent);
    }
    _xfa_hidden_(child, 'parent', pageArea);
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
function _xfa_scope_walk_(from, name) {
    for (var anc = from; anc; anc = anc.parent) {
        var kids = _xfa_kids_of_(anc, name);
        if (kids.length > 0) return kids;
        if (!anc.parent && anc.name === name) return [anc];
    }
    return [];
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
    if (typeof name !== 'string' || name.charAt(0) === '_') return undefined;
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

var _xfa_empty_scope_ = new Proxy({}, { has: function() { return false; } });

// The scope a script owned by `node` runs in (`with` around its function,
// see the engine's `execute_javascript`): unqualified names resolve by
// `_xfa_scope_value_`; anything it does not resolve falls through to the
// global object. A node without links (a path the engine made an object for
// on the fly) uses the nearest registered node on the current path.
function _xfa_scope_(node) {
    if (!node || (!node.parent && node !== xfa.form)) node = _xfa_context_node_() || node;
    if (!node) return _xfa_empty_scope_;
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

// `Name`, `Name[3]`, `Name[*]` -> {name, index}; null for anything else.
function _xfa_parse_segment_(seg) {
    var m = /^([^\[\]#.]+)(?:\[\s*(\*|\d+)\s*\])?$/.exec(seg);
    if (!m) return null;
    return { name: m[1], index: (m[2] === undefined) ? 0 : (m[2] === '*' ? '*' : parseInt(m[2], 10)) };
}

function _xfa_resolve_nodes_(start, expr) {
    expr = String(expr).replace(/^\s+|\s+$/g, '');
    if (expr === '' || expr.indexOf('..') >= 0) return null;
    // `$` is the script's container (XFA 3.3 §3); Acrobat accepts `this`,
    // the JavaScript name for it, the same way.
    if (expr === '$' || expr === 'this') return start ? [start] : [];

    var from = start;
    var absolute = false;
    if (expr.indexOf('$.') === 0) {
        expr = expr.substring(2);
    } else if (expr.indexOf('this.') === 0) {
        expr = expr.substring(5);
    } else if (expr.indexOf('$form.') === 0) {
        expr = expr.substring(6);
        absolute = true;
    } else if (expr.indexOf('xfa.form.') === 0) {
        expr = expr.substring(9);
        absolute = true;
    } else if (expr.charAt(0) === '$' || expr.charAt(0) === '!') {
        return null;
    }

    var parts = expr.split('.');
    var segs = [];
    for (var i = 0; i < parts.length; i++) {
        var seg = _xfa_parse_segment_(parts[i]);
        if (!seg) return null;
        segs.push(seg);
    }

    var selected;
    var first = segs[0];
    if (absolute || !from) {
        var roots = _xfa_kids_of_(xfa.form, first.name);
        if (roots.length === 0) roots = _xfa_kids_of_(_xfa_global_, first.name);
        selected = _xfa_select_(roots, first.index);
    } else {
        selected = _xfa_select_(_xfa_scope_walk_(from, first.name), first.index);
    }

    for (var s = 1; s < segs.length; s++) {
        var next = [];
        for (var k = 0; k < selected.length; k++) {
            next = next.concat(_xfa_select_(_xfa_kids_of_(selected[k], segs[s].name), segs[s].index));
        }
        selected = next;
    }
    return selected;
}

// Run the name-based lookup with `node` as the resolution context, the way a
// script owned by `node` would see it.
function _xfa_legacy_from_(node, legacy, expr) {
    var saved = _xfa_current_context_;
    if (node && node.somExpression) _xfa_current_context_ = node.somExpression;
    try {
        return legacy.call(xfa, expr);
    } finally {
        _xfa_current_context_ = saved;
    }
}

function _xfa_resolve_node_from_(node, expr) {
    var found = _xfa_resolve_nodes_(node, expr);
    if (found && found.length > 0) return found[0];
    return _xfa_legacy_from_(node, xfa._resolveNodeByName, expr);
}

function _xfa_resolve_nodes_from_(node, expr) {
    var found = _xfa_resolve_nodes_(node, expr);
    if (found && found.length > 0) return _xfa_collection_(found);
    var legacy = _xfa_legacy_from_(node, xfa._resolveNodesByName, expr);
    var out = [];
    for (var i = 0; legacy && i < legacy.length; i++) out.push(legacy[i]);
    return _xfa_collection_(out);
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

// The prototype of every form node object. Acrobat throws when a script
// reads a property an XFA object does not have, typically a child that is
// not there, and forms rely on it: AALP_019_EN tries
// `xfa.form.UBSForms.UBSForms_<id>` inside `try` to fall back to the root.
// Names on `Object.prototype`, symbols, engine-internal `_` names and the
// XFA names above are answered normally; everything else throws.
var _xfa_node_proto_ = new Proxy(Object.create(Object.prototype), {
    get: function(target, key, receiver) {
        if (typeof key !== 'string' || key in Object.prototype) {
            return Reflect.get(target, key, receiver);
        }
        if (key.charAt(0) === '_' || key.charAt(0) === '$'
            || _xfa_node_property_names_[key] === true) {
            return undefined;
        }
        var where = Object.prototype.hasOwnProperty.call(receiver, 'somExpression')
            ? String(receiver.somExpression) : 'this object';
        throw new Error("Invalid property get operation; " + where
            + " doesn't have property '" + key + "'");
    }
});

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
        parent[this._childName] = this._instances[0];
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
        "{XFA_RESOLVE_PATH_HELPER}\n{XFA_SOM_WALKER}\n{XFA_INSTANCE_MANAGER}\n{XFA_HOST_EXTENSIONS}\n{XFA_VALUE_FORMAT}\n{XFA_SCRIPT_OBJECTS}\n{XFA_FORMCALC_RUNTIME}"
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

/// Named script objects (`<variables><script name>`), instantiated on first
/// reference (XFA 3.3 §10 "Instantiation of Named Script Objects": "when the
/// document variable is referenced the compiled script object is
/// returned").
///
/// Instantiating in document order instead breaks the UBS idiom
/// `try{ myDE; } catch(e){ soCommonLabelDefinition; }` at the top of an
/// object declared before `soCommonLabelDefinition`: referencing the later
/// object is what makes it exist.
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
    return kids.length > 0 ? kids[0] : base[name];
}

function _fc_index_(base, name, i) {
    var found = _xfa_select_(_xfa_kids_of_(base, name), i);
    if (found.length === 0) throw new Error("FormCalc: no " + name + "[" + i + "]");
    return found[0];
}

function _fc_first_(node, name, i) {
    var found = _xfa_select_(_xfa_scope_walk_(node, name), i);
    if (found.length === 0) throw new Error("FormCalc: no " + name + "[" + i + "]");
    return found[0];
}

function _fc_set_node_(target, v) {
    if (!_fc_is_node_(target)) throw new Error("FormCalc: assignment to a value, not a node");
    target.rawValue = v;
    return v;
}

function _fc_set_(base, name, v) {
    var kids = _xfa_kids_of_(base, name);
    if (kids.length > 0) kids[0].rawValue = v; else base[name] = v;
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
