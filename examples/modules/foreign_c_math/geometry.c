#include <stdint.h>
#include <math.h>

double circle_area(double radius) {
    return radius * radius * 3.14159265358979;
}

double rectangle_area(double width, double height) {
    return width * height;
}

double hypotenuse(double a, double b) {
    return sqrt(a * a + b * b);
}

int64_t clamp(int64_t value, int64_t min, int64_t max) {
    if (value < min) return min;
    if (value > max) return max;
    return value;
}
