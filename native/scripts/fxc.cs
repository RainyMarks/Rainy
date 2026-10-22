// An fxc.exe for gpui-pre-windows' release build script, which precompiles its HLSL with the Windows SDK's fxc.
// fxc is a front end for d3dcompiler_47.dll, which every Windows 10 and 11 ships, so this compiles the same way
// without the SDK. It takes the arguments the build script passes (/T, /E, /Fh, /Vn, /O0-/O3, the file) and writes
// the header fxc would write. Built by native/scripts/build-release.sh with the C# compiler of .NET Framework.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

[ComImport, Guid("8BA5FB08-5195-40e2-AC58-0D989C3A0102"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface ID3DBlob {
    [PreserveSig] IntPtr GetBufferPointer();
    [PreserveSig] UIntPtr GetBufferSize();
}

static class Fxc {
    [DllImport("d3dcompiler_47.dll", CharSet = CharSet.Unicode)]
    static extern int D3DCompileFromFile(string file, IntPtr defines, IntPtr include,
        [MarshalAs(UnmanagedType.LPStr)] string entryPoint, [MarshalAs(UnmanagedType.LPStr)] string target,
        uint flags1, uint flags2, out ID3DBlob code, out ID3DBlob errors);

    const uint SkipOptimization = 1 << 2;
    const uint OptimizationLevel0 = 1 << 14;
    const uint OptimizationLevel2 = (1 << 14) | (1 << 15);
    const uint OptimizationLevel3 = 1 << 15;
    // D3D_COMPILE_STANDARD_FILE_INCLUDE: #include resolves relative to the shader file.
    static readonly IntPtr StandardInclude = new IntPtr(1);

    static byte[] Bytes(ID3DBlob blob) {
        if (blob == null) return new byte[0];
        var bytes = new byte[(int)blob.GetBufferSize().ToUInt64()];
        Marshal.Copy(blob.GetBufferPointer(), bytes, 0, bytes.Length);
        return bytes;
    }

    static int Main(string[] args) {
        string target = null, entry = null, header = null, name = null, file = null;
        uint flags = 0;
        for (int i = 0; i < args.Length; i++) {
            string arg = args[i];
            switch (arg) {
                case "/T": target = args[++i]; break;
                case "/E": entry = args[++i]; break;
                case "/Fh": header = args[++i]; break;
                case "/Vn": name = args[++i]; break;
                case "/Od": flags |= SkipOptimization; break;
                case "/O0": flags |= OptimizationLevel0; break;
                case "/O1": break;
                case "/O2": flags |= OptimizationLevel2; break;
                case "/O3": flags |= OptimizationLevel3; break;
                default:
                    if (arg.StartsWith("/")) { Console.Error.WriteLine("fxc: unsupported option " + arg); return 2; }
                    file = arg;
                    break;
            }
        }
        if (target == null || entry == null || header == null || file == null) {
            Console.Error.WriteLine("usage: fxc /T <target> /E <entry> /Fh <header> [/Vn <name>] [/O0-/O3] <file>");
            return 2;
        }
        ID3DBlob code, errors;
        int result = D3DCompileFromFile(Path.GetFullPath(file), IntPtr.Zero, StandardInclude, entry, target, flags, 0, out code, out errors);
        string messages = Encoding.ASCII.GetString(Bytes(errors)).TrimEnd('\0');
        if (result < 0) {
            Console.Error.WriteLine(messages.Length > 0 ? messages : string.Format("fxc: compilation failed (0x{0:X8})", result));
            return 1;
        }
        if (messages.Length > 0) Console.Error.WriteLine(messages);
        byte[] bytecode = Bytes(code);
        var text = new StringBuilder();
        text.Append("const BYTE ").Append(name ?? "g_" + entry).Append("[] =\n{\n");
        for (int i = 0; i < bytecode.Length; i++) {
            text.Append(i % 16 == 0 ? "    " : " ").Append(bytecode[i]).Append(i + 1 < bytecode.Length ? "," : "");
            if (i % 16 == 15 || i + 1 == bytecode.Length) text.Append('\n');
        }
        text.Append("};\n");
        File.WriteAllText(header, text.ToString(), new UTF8Encoding(false));
        return 0;
    }
}
