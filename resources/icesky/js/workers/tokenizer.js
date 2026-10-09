/** Token display semantics shared with the original IceSky tokenizer. */
(function (scope) {
const analyzeToken = function (e) { if ("number" == typeof e.byteValue) {
            const t = e.byteValue;
            let n = "", o = "normal", i = e.text || `0x${t.toString(16).padStart(2, "0")}`;
            return 32 === t ? (n = "空格", o = "space", i += " ␠") : 9 === t ? (n = "Tab", o = "tab", i += " ⇥") : 10 === t ? (n = "换行", o = "newline", i += " ↵") : 13 === t ? (n = "回车", o = "newline", i += " ␍") : (t >= 0 && t <= 31 || 127 === t) && (n = "控制", o = "control"), { display: i, title: n ? `${i}\n特殊token：${n}` : i, special: !!n, kind: o, tag: n };
        } const t = "string" == typeof e.text ? e.text : String(e.text || ""), n = [], o = /[\u200B-\u200D\u2060\uFEFF]/u.test(t), i = /[\uFE00-\uFE0F]/u.test(t), r = /[\r\n]/.test(t), s = /\t/.test(t), a = /[ \u00A0]/.test(t), u = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/.test(t); o && n.push("零宽"), i && n.push("变体"), r && n.push("换行"), s && n.push("Tab"), a && n.push("空格"), u && n.push("控制"); const l = t.replace(/\u200B/g, "[ZWSP]").replace(/\u200C/g, "[ZWNJ]").replace(/\u200D/g, "[ZWJ]").replace(/\u2060/g, "[WJ]").replace(/\uFEFF/g, "[BOM]").replace(/\uFE0E/g, "[VS15]").replace(/\uFE0F/g, "[VS16]").replace(/\r/g, "␍").replace(/\n/g, "↵").replace(/\t/g, "⇥").replace(/ /g, "␠").replace(/\u00A0/g, "⍽"); let c = "normal"; return n.length && (c = "空格" === n[0] ? "space" : "Tab" === n[0] ? "tab" : "换行" === n[0] ? "newline" : "零宽" === n[0] ? "zero-width" : "变体" === n[0] ? "variation" : "控制" === n[0] ? "control" : "special"), { display: l || "∅", title: n.length ? `原始token：${t || "(空)"}\n特殊token：${n.join(" / ")}` : t, special: n.length > 0, kind: c, tag: n[0] || "" }; };
scope.IceSkyAnalyzeToken = analyzeToken;
const PAGE_SIZE = 100;

/** Retains compact token values and materializes only the requested page. */
class TokenizerResult {
    constructor(input, engine) {
        this.input = input;
        this.engine = engine;
        this.values = null;
        this.encoding = null;
        this.charOffsets = null;
        this.byteOffsets = null;
        this.charCount = 0;
        this.wordCount = (input.trim().match(/[^\s]+/g) || []).length;
        this.specialCount = 0;
        this.specialBreakdown = [];
    }

    async initialize() {
        for (const unused of this.input) this.charCount++;
        if (this.engine === 'byte') {
            this.values = new TextEncoder().encode(this.input);
            this.charOffsets = new Uint32Array(this.charCount + 1);
            this.byteOffsets = new Uint32Array(this.charCount + 1);
            let index = 0, charOffset = 0, byteOffset = 0;
            for (const character of this.input) {
                this.charOffsets[index] = charOffset;
                this.byteOffsets[index++] = byteOffset;
                charOffset += character.length;
                const code = character.codePointAt(0);
                byteOffset += code <= 0x7f ? 1 : code <= 0x7ff ? 2 : code <= 0xffff ? 3 : 4;
            }
            this.charOffsets[index] = charOffset;
            this.byteOffsets[index] = byteOffset;
        } else if (this.engine === 'word') {
            this.values = this.input.split(/(\s+|[\.,!?:;()\[\]{}])/).filter(Boolean);
        } else {
            this.encoding = await scope.TokenizerRuntime.load(this.engine);
            this.values = Uint32Array.from(this.encoding.encode(this.input));
        }
        const breakdown = new Map();
        for (let index = 0; index < this.values.length; index++) {
            let tag;
            if (this.engine === 'byte') {
                const value = this.values[index];
                tag = value === 32 ? '空格' : value === 9 ? 'Tab' : value === 10 ? '换行' : value === 13 ? '回车' : value <= 31 || value === 127 ? '控制' : '';
            } else {
                const details = analyzeToken(this.rawToken(index));
                tag = details.special ? details.tag || '特殊' : '';
            }
            if (!tag) continue;
            this.specialCount++;
            breakdown.set(tag, (breakdown.get(tag) || 0) + 1);
        }
        this.specialBreakdown = Array.from(breakdown, ([label, count]) => ({ label, count })).sort((a, b) => b.count - a.count);
        return this;
    }

    rawToken(index) {
        const value = this.values[index];
        if (this.engine === 'byte') return { id: value, byteValue: value, text: `0x${value.toString(16).padStart(2, '0')}` };
        if (this.engine === 'word') return { text: value };
        return { id: value, text: this.encoding.decode([value]) };
    }

    token(index) { const raw = this.rawToken(index); return { ...raw, ...analyzeToken(raw), index }; }

    page(requestedPage) {
        const totalCount = this.values.length;
        const itemCount = this.engine === 'byte' ? this.charCount : totalCount;
        const pageCount = Math.max(1, Math.ceil(itemCount / PAGE_SIZE));
        const page = Math.min(pageCount - 1, Math.max(0, requestedPage));
        const start = page * PAGE_SIZE, end = Math.min(itemCount, start + PAGE_SIZE);
        const tokens = [], groups = [];
        if (this.engine === 'byte') {
            for (let index = start; index < end; index++) {
                const text = this.input.slice(this.charOffsets[index], this.charOffsets[index + 1]);
                const groupTokens = [];
                for (let tokenIndex = this.byteOffsets[index]; tokenIndex < this.byteOffsets[index + 1]; tokenIndex++) groupTokens.push(this.token(tokenIndex));
                groups.push({ display: analyzeToken({ text }).display, codePoint: `U+${text.codePointAt(0).toString(16).toUpperCase().padStart(4, '0')}`, start: this.byteOffsets[index], index, tokens: groupTokens });
            }
        } else {
            for (let index = start; index < end; index++) tokens.push(this.token(index));
        }
        return { tokens, groups, totalCount, page, pageCount, charCount: this.charCount, wordCount: this.wordCount, specialCount: this.specialCount, specialBreakdown: this.specialBreakdown };
    }
}

scope.IceSkyTokenizerResult = TokenizerResult;
})(globalThis);
