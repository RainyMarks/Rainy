/** Resolves declared defaults and the caller's explicit, context-owned preferences. */
function getMergedTransformOptions(transform, preferences = {}) {
    if (!transform || !Array.isArray(transform.configurableOptions) || !transform.configurableOptions.length) return {};
    const defaults = {};
    for (const option of transform.configurableOptions) {
        let value = option.default;
        if (value == null) value = option.type === 'boolean' ? false : option.type === 'select' && option.options?.length ? option.options[0].value : option.type === 'number' ? 0 : '';
        defaults[option.id] = value;
    }
    return Object.assign(defaults, preferences[transform.name] || {});
}

/** Finds transform metadata without reading browser-origin storage. */
function getMergedTransformOptionsForName(name, transforms, preferences = {}) {
    if (!name) return {};
    let transform = Array.isArray(transforms) ? transforms.find(item => item && item.name === name) : null;
    if (!transform?.configurableOptions?.length && typeof window !== 'undefined' && window.transforms) {
        transform = Object.values(window.transforms).find(item => item && item.name === name) || transform;
    }
    return getMergedTransformOptions(transform, preferences);
}

if (typeof window !== 'undefined') Object.assign(window, { getMergedTransformOptions, getMergedTransformOptionsForName });
if (typeof module !== 'undefined' && module.exports) module.exports = { getMergedTransformOptions, getMergedTransformOptionsForName };
