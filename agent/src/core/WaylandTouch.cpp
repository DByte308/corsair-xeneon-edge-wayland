// SPDX-License-Identifier: GPL-3.0-or-later
#include "core/WaylandTouch.h"
#include <QDBusInterface>
#include <QDBusReply>
#include <QDBusVariant>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QProcess>

namespace xen::wayland {
namespace {
constexpr auto service = "org.kde.KWin";
constexpr auto iface = "org.kde.KWin.InputDevice";
QString path(const QString& id) { return QStringLiteral("/org/kde/KWin/InputDevice/") + id; }
QVariant property(const QString& id, const QString& key)
{
    QDBusInterface dev(service, path(id), "org.freedesktop.DBus.Properties", QDBusConnection::sessionBus());
    QDBusReply<QDBusVariant> reply = dev.call("Get", QString::fromLatin1(iface), key);
    return reply.isValid() ? reply.value().variant() : QVariant();
}
bool set(const QString& id, const QString& key, const QVariant& value)
{
    QDBusInterface dev(service, path(id), "org.freedesktop.DBus.Properties", QDBusConnection::sessionBus());
    QDBusReply<void> reply = dev.call("Set", QString::fromLatin1(iface), key,
                                    QVariant::fromValue(QDBusVariant(value)));
    return reply.isValid() && property(id, key) == value;
}
}
bool active() { return qEnvironmentVariable("XDG_SESSION_TYPE").compare("wayland", Qt::CaseInsensitive) == 0; }
QStringList devices()
{
    QDBusInterface manager(service, "/org/kde/KWin/InputDevice", "org.kde.KWin.InputDeviceManager", QDBusConnection::sessionBus());
    QDBusReply<QStringList> reply = manager.call("ListTouch");
    QStringList result;
    if (!reply.isValid()) return result;
    for (const auto& id : reply.value()) {
        // Match the actual digitizer, never an unrelated touchscreen.
        if (property(id, "vendor").toUInt() == 0x27c0
            && property(id, "product").toUInt() == 0x0859
            && property(id, "name").toString() == "wch.cn TouchScreen") result.append(id);
    }
    return result;
}
bool enabled(const QString& id) { return property(id, "enabled").toBool(); }
bool setEnabled(bool value)
{
    const auto ids = devices();
    if (ids.isEmpty()) return false;
    bool ok = true;
    for (const auto& id : ids) if (!set(id, "enabled", value)) ok = false;
    return ok;
}
QString edgeOutput()
{
    QProcess p;
    p.start("kscreen-doctor", {"-j"});
    if (!p.waitForStarted(1500) || !p.waitForFinished(3000)
        || p.exitStatus() != QProcess::NormalExit || p.exitCode() != 0) return {};
    const auto outputs = QJsonDocument::fromJson(p.readAllStandardOutput()).object().value("outputs").toArray();
    QStringList matches;
    for (const auto& value : outputs) {
        const auto o = value.toObject();
        if (!o.value("connected").toBool() || !o.value("enabled").toBool()) continue;
        for (const auto& mode : o.value("modes").toArray()) {
            const auto m = mode.toObject();
            const auto size = m.value("size").toObject();
            if (m.value("id") == o.value("currentModeId")
                && size.value("width").toInt() == 2560 && size.value("height").toInt() == 720)
                matches.append(o.value("name").toString());
        }
    }
    // Refuse ambiguous mappings rather than steal another display's touch.
    return matches.size() == 1 ? matches.first() : QString();
}
bool mapToOutput()
{
    const auto output = edgeOutput();
    const auto ids = devices();
    if (output.isEmpty() || ids.isEmpty()) return false;
    bool ok = true;
    for (const auto& id : ids) if (!set(id, "outputName", output)) ok = false;
    return ok;
}
}
