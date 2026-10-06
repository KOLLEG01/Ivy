using System.Buffers.Binary;
using System.Text;

namespace Ivy.PhoneBridge;

// USB/IP/HID descriptor layout adapted from codexdeck d8d8e737, usbip-device.ts.
// See licenses/CodexMicro-MIT.txt. The serial/bus identify Ivy, not a physical macro-pad.
public static class MicroUsbDescriptors {
    public const string BusId = "1-7";
    // The installed Windows USB/IP client accepts at most 15 ASCII letters/digits.
    public const string Serial = "IvyPhoneMicro";
    public const uint DeviceId = 0x00010007;
    public static byte[] Report() => [
        0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x06, 0x09, 0x01,
        0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x3f, 0x81, 0x02,
        0x09, 0x01, 0x91, 0x02, 0xc0
    ];
    public static byte[] Device() => [
        0x12, 0x01, 0x00, 0x02, 0, 0, 0, 0x40, 0x3a, 0x30, 0x60, 0x83,
        0, 1, 1, 2, 3, 1
    ];
    public static byte[] Configuration() => [
        9, 2, 41, 0, 1, 1, 0, 0x80, 0x32,
        9, 4, 0, 0, 2, 3, 0, 0, 0,
        9, 0x21, 0x11, 1, 0, 1, 0x22, 27, 0,
        7, 5, 0x81, 3, 64, 0, 1,
        7, 5, 0x01, 3, 64, 0, 1
    ];
    public static byte[] String(int index) {
        if (index == 0) return [4, 3, 9, 4];
        string text = index switch { 1 => "Work Louder", 2 => "Codex Micro", 3 => Serial, _ => null };
        if (text == null) return null;
        byte[] value = Encoding.Unicode.GetBytes(text), result = new byte[value.Length + 2];
        result[0] = (byte)result.Length; result[1] = 3; value.CopyTo(result, 2); return result;
    }
    public static byte[] Export() {
        var value = new byte[312];
        Encoding.ASCII.GetBytes("/sys/devices/ivy-phone/" + BusId).CopyTo(value, 0);
        Encoding.ASCII.GetBytes(BusId).CopyTo(value, 256);
        BinaryPrimitives.WriteUInt32BigEndian(value.AsSpan(288), 1);
        BinaryPrimitives.WriteUInt32BigEndian(value.AsSpan(292), 7);
        BinaryPrimitives.WriteUInt32BigEndian(value.AsSpan(296), 3);
        BinaryPrimitives.WriteUInt16BigEndian(value.AsSpan(300), 0x303a);
        BinaryPrimitives.WriteUInt16BigEndian(value.AsSpan(302), 0x8360);
        BinaryPrimitives.WriteUInt16BigEndian(value.AsSpan(304), 0x0100);
        value[309] = value[310] = value[311] = 1; return value;
    }
}
