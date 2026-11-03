// SPDX-License-Identifier: GPL-3.0-or-later
// Read-only hardware probe: never changes touch or display configuration.
#include "core/WaylandTouch.h"
#include <QCoreApplication>
#include <QTextStream>
int main(int argc, char** argv)
{
    QCoreApplication app(argc, argv);
    QTextStream out(stdout);
    out << "Wayland: " << xen::wayland::active() << '\n';
    const auto ids = xen::wayland::devices();
    out << "Edge digitizers: " << ids.join(',') << '\n';
    const auto output = xen::wayland::edgeOutput();
    out << "Edge output: " << output << '\n';
    for (const auto& id : ids) out << id << " enabled: " << xen::wayland::enabled(id) << '\n';
    return xen::wayland::active() && !ids.isEmpty() && !output.isEmpty() ? 0 : 1;
}
