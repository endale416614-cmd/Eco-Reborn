// Game-window test helper for Eco Reborn UI tests.
// Captures and sends input only to the eco.exe window (by handle), never to the desktop,
// so nothing reaches other applications even when the game is not in front.
//
//   eco-window find                 window handle, client size, process id
//   eco-window capture <out.png>    screenshot of the game's client area
//   eco-window click <x> <y> [right|double]   client-area coordinates
//   eco-window key <name|vk> [count]          e.g. enter, esc, tab, f1, 0x41
//   eco-window type <text>          WM_CHAR per character
//   eco-window borderless [on|off]  strip/restore the window frame and fill the monitor
//   eco-window watch                keep the game borderless until it exits
using System;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Threading;

internal static class EcoWindow
{
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
    [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int index);
    [DllImport("user32.dll")] static extern int SetWindowLong(IntPtr h, int index, int value);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr h, uint flags);
    [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr m, ref MONITORINFO info);

    [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct MONITORINFO { public int Size; public RECT Monitor, Work; public uint Flags; }

    const int GWL_STYLE = -16;
    const int WS_CAPTION = 0x00C00000, WS_THICKFRAME = 0x00040000, WS_SYSMENU = 0x00080000,
        WS_MINIMIZEBOX = 0x00020000, WS_MAXIMIZEBOX = 0x00010000, WS_POPUP = unchecked((int)0x80000000);
    const uint SWP_FRAMECHANGED = 0x0020, SWP_NOZORDER = 0x0004, SWP_SHOWWINDOW = 0x0040;
    const uint WM_KEYDOWN = 0x0100, WM_KEYUP = 0x0101, WM_CHAR = 0x0102, WM_MOUSEMOVE = 0x0200,
        WM_LBUTTONDOWN = 0x0201, WM_LBUTTONUP = 0x0202, WM_LBUTTONDBLCLK = 0x0203,
        WM_RBUTTONDOWN = 0x0204, WM_RBUTTONUP = 0x0205;

    static int Main(string[] args)
    {
        if (args.Length == 0) return Usage();
        if (args[0] == "watch") return Watch();
        int pid;
        IntPtr window = FindGame(out pid);
        if (window == IntPtr.Zero) { Console.Error.WriteLine("eco.exe window not found"); return 3; }
        switch (args[0])
        {
            case "find":
                RECT r;
                GetClientRect(window, out r);
                Console.WriteLine("hwnd=0x{0:X} pid={1} client={2}x{3}", window.ToInt64(), pid, r.Right, r.Bottom);
                return 0;
            case "capture": return args.Length == 2 ? Capture(window, args[1]) : Usage();
            case "click": return args.Length >= 3 ? Click(window, int.Parse(args[1]), int.Parse(args[2]), args.Length > 3 ? args[3] : "left") : Usage();
            case "key": return args.Length >= 2 ? Key(window, args[1], args.Length > 2 ? int.Parse(args[2]) : 1) : Usage();
            case "type": return args.Length >= 2 ? Type(window, string.Join(" ", args, 1, args.Length - 1)) : Usage();
            case "borderless": return Borderless(window, args.Length < 2 || args[1] != "off");
            default: return Usage();
        }
    }

    static int Usage()
    {
        Console.Error.WriteLine("usage: eco-window find | capture <out.png> | click <x> <y> [left|right|double] | key <name|vk> [count] | type <text> | borderless [on|off] | watch");
        return 2;
    }

    // Keeps the game borderless while it runs. The client puts its frame back whenever it
    // switches between windowed and full screen in Setup; exclusive full screen has no
    // caption, so it is left alone. Exits when the game closes.
    static int Watch()
    {
        DateTime started = DateTime.UtcNow;
        bool seen = false;
        int missing = 0;
        while (true)
        {
            int pid;
            IntPtr window = FindGame(out pid);
            if (window == IntPtr.Zero)
            {
                if (seen && ++missing > 20) return 0;
                if (!seen && (DateTime.UtcNow - started).TotalSeconds > 120) return 3;
            }
            else
            {
                seen = true;
                missing = 0;
                if ((GetWindowLong(window, GWL_STYLE) & WS_CAPTION) != 0) Borderless(window, true);
            }
            Thread.Sleep(500);
        }
    }

    static IntPtr FindGame(out int pid)
    {
        pid = 0;
        foreach (Process p in Process.GetProcessesByName("eco"))
        {
            IntPtr h = p.MainWindowHandle;
            if (h != IntPtr.Zero && IsWindowVisible(h)) { pid = p.Id; return h; }
        }
        return IntPtr.Zero;
    }

    static int Capture(IntPtr window, string path)
    {
        RECT r;
        GetClientRect(window, out r);
        if (r.Right <= 0 || r.Bottom <= 0) { Console.Error.WriteLine("window has no client area (minimised?)"); return 4; }
        using (Bitmap whole = new Bitmap(r.Right, r.Bottom, PixelFormat.Format32bppArgb))
        {
            // A capture taken while the game presents a frame can come back blank white; retry.
            for (int attempt = 0; attempt < 5; attempt++)
            {
                using (Graphics g = Graphics.FromImage(whole))
                {
                    IntPtr dc = g.GetHdc();
                    // PW_CLIENTONLY | PW_RENDERFULLCONTENT: works for Direct3D windows even when covered.
                    bool ok = PrintWindow(window, dc, 0x1 | 0x2);
                    g.ReleaseHdc(dc);
                    if (!ok) { Console.Error.WriteLine("PrintWindow failed"); return 5; }
                }
                if (!IsBlank(whole)) break;
                Thread.Sleep(150);
            }
            whole.Save(path, ImageFormat.Png);
        }
        Console.WriteLine("{0} ({1}x{2})", path, r.Right, r.Bottom);
        return 0;
    }

    static bool IsBlank(Bitmap image)
    {
        Color first = image.GetPixel(0, 0);
        for (int y = 0; y < image.Height; y += image.Height / 8 + 1)
            for (int x = 0; x < image.Width; x += image.Width / 8 + 1)
                if (image.GetPixel(x, y) != first) return false;
        return true;
    }

    static IntPtr Pack(int lo, int hi) { return (IntPtr)((hi << 16) | (lo & 0xFFFF)); }

    static int Click(IntPtr window, int x, int y, string kind)
    {
        IntPtr at = Pack(x, y);
        PostMessage(window, WM_MOUSEMOVE, IntPtr.Zero, at);
        Thread.Sleep(30);
        if (kind == "right")
        {
            PostMessage(window, WM_RBUTTONDOWN, (IntPtr)0x2, at); Thread.Sleep(40);
            PostMessage(window, WM_RBUTTONUP, IntPtr.Zero, at);
        }
        else
        {
            PostMessage(window, WM_LBUTTONDOWN, (IntPtr)0x1, at); Thread.Sleep(40);
            PostMessage(window, WM_LBUTTONUP, IntPtr.Zero, at);
            if (kind == "double")
            {
                Thread.Sleep(40);
                PostMessage(window, WM_LBUTTONDBLCLK, (IntPtr)0x1, at); Thread.Sleep(40);
                PostMessage(window, WM_LBUTTONUP, IntPtr.Zero, at);
            }
        }
        return 0;
    }

    static int Key(IntPtr window, string name, int count)
    {
        uint vk = VirtualKey(name);
        if (vk == 0) { Console.Error.WriteLine("unknown key " + name); return 2; }
        uint scan = MapVirtualKey(vk, 0);
        for (int i = 0; i < count; i++)
        {
            PostMessage(window, WM_KEYDOWN, (IntPtr)vk, (IntPtr)(1 | (scan << 16)));
            Thread.Sleep(40);
            PostMessage(window, WM_KEYUP, (IntPtr)vk, (IntPtr)unchecked((int)(1 | (scan << 16) | 0xC0000000)));
            Thread.Sleep(60);
        }
        return 0;
    }

    static uint VirtualKey(string name)
    {
        switch (name.ToLowerInvariant())
        {
            case "enter": return 0x0D; case "esc": case "escape": return 0x1B; case "tab": return 0x09;
            case "space": return 0x20; case "backspace": return 0x08; case "up": return 0x26; case "down": return 0x28;
            case "left": return 0x25; case "right": return 0x27; case "alt": return 0x12; case "ctrl": return 0x11;
            case "shift": return 0x10; case "insert": return 0x2D; case "delete": return 0x2E;
            case "home": return 0x24; case "end": return 0x23; case "pageup": return 0x21; case "pagedown": return 0x22;
        }
        int f;
        if (name.Length > 1 && (name[0] == 'f' || name[0] == 'F') && int.TryParse(name.Substring(1), out f) && f >= 1 && f <= 12)
            return (uint)(0x6F + f);
        if (name.StartsWith("0x")) return Convert.ToUInt32(name.Substring(2), 16);
        if (name.Length == 1) return (uint)char.ToUpperInvariant(name[0]);
        return 0;
    }

    static int Type(IntPtr window, string text)
    {
        foreach (char c in text) { PostMessage(window, WM_CHAR, (IntPtr)c, (IntPtr)1); Thread.Sleep(30); }
        return 0;
    }

    static int Borderless(IntPtr window, bool on)
    {
        int style = GetWindowLong(window, GWL_STYLE);
        int frame = WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        MONITORINFO info = new MONITORINFO();
        info.Size = Marshal.SizeOf(typeof(MONITORINFO));
        GetMonitorInfo(MonitorFromWindow(window, 2), ref info);
        RECT m = info.Monitor;
        if (on)
        {
            SetWindowLong(window, GWL_STYLE, (style & ~frame) | WS_POPUP);
            SetWindowPos(window, IntPtr.Zero, m.Left, m.Top, m.Right - m.Left, m.Bottom - m.Top, SWP_FRAMECHANGED | SWP_NOZORDER | SWP_SHOWWINDOW);
        }
        else
        {
            SetWindowLong(window, GWL_STYLE, (style & ~WS_POPUP) | frame);
            SetWindowPos(window, IntPtr.Zero, m.Left + 40, m.Top + 40, (m.Right - m.Left) * 3 / 4, (m.Bottom - m.Top) * 3 / 4, SWP_FRAMECHANGED | SWP_NOZORDER | SWP_SHOWWINDOW);
        }
        Console.WriteLine("borderless " + (on ? "on" : "off") + " ({0}x{1})", m.Right - m.Left, m.Bottom - m.Top);
        return 0;
    }
}
