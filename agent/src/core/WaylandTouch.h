// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <QString>
#include <QStringList>

namespace xen::wayland {
bool active();
QStringList devices();
bool enabled(const QString& device);
bool setEnabled(bool value);
QString edgeOutput();
bool mapToOutput();
}
