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
        if (obj && typeof obj[part] !== 'undefined') {
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
    var direct = obj[name];
    if (direct !== null && typeof direct === 'object') return [direct];
    return [];
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
    if (expr === '$') return start ? [start] : [];

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
        selected = [];
        for (var anc = from; anc; anc = anc.parent) {
            var kids = _xfa_kids_of_(anc, first.name);
            if (kids.length > 0) {
                selected = _xfa_select_(kids, first.index);
                break;
            }
            if (!anc.parent && anc.name === first.name) {
                selected = _xfa_select_([anc], first.index);
                break;
            }
        }
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

function _xfa_install_node_methods_(obj) {
    _xfa_hidden_(obj, 'resolveNode', _xfa_node_resolveNode_);
    _xfa_hidden_(obj, 'resolveNodes', _xfa_node_resolveNodes_);
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
    format!("{XFA_RESOLVE_PATH_HELPER}\n{XFA_SOM_WALKER}\n{XFA_INSTANCE_MANAGER}")
}
