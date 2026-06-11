import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_13108 extends AI {
public class u_A extends NativeObjectLeekValue {
public Double x;
public u_A() throws LeekRunException {
allocateRAM(this, 2);
x = realOrNull(null);
}
public u_A(u_A o, int level) throws LeekRunException {
this.x = level == 1 ? o.x : (Double) copy(o.x, level - 1);
}
public Object init() throws LeekRunException {
return null;
}
public Object u_m() throws LeekRunException {
ops(x = (double)(long) 5l, 2);
return null;
}
}
public ClassLeekValue u_A = new ClassLeekValue(this, "A", null, u_A.class);
public u_A new_u_A(Object... args) throws LeekRunException {
return (u_A) execute(u_A, args);
}
public AI_13108() throws LeekRunException {
super(3, 3);
u_A.initFields = new FunctionLeekValue(0) {public Object run(AI ai, Object u_this, Object... values) throws LeekRunException {
return null;
}};
u_A.addMethod("m", 0, new FunctionLeekValue(0) { public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {
return ((u_A) thiz).u_m(); }}, AccessLevel.PUBLIC);
u_A.addGenericMethod("m");
}
private void createStaticClass_A() throws LeekRunException {
}
private void initClass_A() throws LeekRunException {
}
public void staticInit() throws LeekRunException {
createStaticClass_A();
initClass_A();
}
public Object runIA(Session session) throws LeekRunException {
Object u_a = ops(new_u_A(), 1);
ops(callObjectAccess(u_a, "m", "u_m", null), 1);
return ops(getField(u_a, "x", null), 1);
}
protected String getAIString() { return "<snippet 13108>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 13108>", };}

protected int[] getErrorFilesID() { return new int[] {13108, };}

}
