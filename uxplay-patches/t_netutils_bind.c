/* Self-check for netutils_parse_ipv4 / netutils_set_bind_address.
 * Build: cc -I<UxPlay>/lib t_bind.c <UxPlay>/lib/netutils.c -o t_bind && ./t_bind */
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <arpa/inet.h>
#include "netutils.h"

int main(void) {
    unsigned int a;

    /* accepts a canonical dotted quad, network byte order */
    assert(netutils_parse_ipv4("192.168.1.50", &a) == 0);
    assert(a == inet_addr("192.168.1.50"));
    assert(netutils_parse_ipv4("0.0.0.0", &a) == 0 && a == 0);
    assert(netutils_parse_ipv4("255.255.255.255", &a) == 0 && a == 0xffffffffu);
    assert(netutils_parse_ipv4("10.0.0.1", &a) == 0 && a == inet_addr("10.0.0.1"));

    /* the trust boundary: everything a hand-edited config.json could smuggle in */
    assert(netutils_parse_ipv4(" 192.168.1.50", &a) < 0);   /* leading space */
    assert(netutils_parse_ipv4("192.168.1.50 ", &a) < 0);   /* trailing space */
    assert(netutils_parse_ipv4("192.168.1.50 -nohold", &a) < 0);
    assert(netutils_parse_ipv4("+1.2.3.4", &a) < 0);
    assert(netutils_parse_ipv4("-1.2.3.4", &a) < 0);
    assert(netutils_parse_ipv4("1.2.3", &a) < 0);
    assert(netutils_parse_ipv4("1.2.3.4.5", &a) < 0);
    assert(netutils_parse_ipv4("1.2.3.256", &a) < 0);
    assert(netutils_parse_ipv4("1.2.3.0004", &a) < 0);      /* > 3 digits */
    assert(netutils_parse_ipv4("1..2.3", &a) < 0);
    assert(netutils_parse_ipv4("1.2.3.", &a) < 0);
    assert(netutils_parse_ipv4(".1.2.3", &a) < 0);
    assert(netutils_parse_ipv4("", &a) < 0);
    assert(netutils_parse_ipv4("eth0", &a) < 0);
    assert(netutils_parse_ipv4("::1", &a) < 0);
    assert(netutils_parse_ipv4(NULL, &a) < 0);
    assert(netutils_parse_ipv4("1.2.3.4", NULL) < 0);

    /* F1: the statics outlive an engine restart, so a reset must really reset */
    assert(!strcmp(netutils_get_bind_host(), "localhost"));
    assert(netutils_set_bind_address("192.168.1.50") == 0);
    assert(!strcmp(netutils_get_bind_host(), "192.168.1.50"));
    assert(netutils_set_bind_address("nonsense") < 0);
    assert(!strcmp(netutils_get_bind_host(), "192.168.1.50"));  /* failed set keeps the old pin */
    assert(netutils_set_bind_address(NULL) == 0);
    assert(!strcmp(netutils_get_bind_host(), "localhost"));
    assert(netutils_set_bind_address("") == 0);
    assert(!strcmp(netutils_get_bind_host(), "localhost"));

    printf("t_bind: all assertions passed\n");
    return 0;
}
