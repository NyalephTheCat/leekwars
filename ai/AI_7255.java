import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_7255 extends AI {
public class u_A extends ArrayLeekValue {
public u_A() throws LeekRunException {
super(AI_7255.this);
allocateRAM(this, 0);
}
public u_A(u_A o, int level) throws LeekRunException {
super(AI_7255.this, o, level);
}
public Object init() throws LeekRunException {
return null;
}
}
public ClassLeekValue u_A = new ClassLeekValue(this, "A", null, u_A.class);
public u_A new_u_A(Object... args) throws LeekRunException {
return (u_A) execute(u_A, args);
}
public AI_7255() throws LeekRunException {
super(0, 4);
u_A.setParent(arrayClass);
u_A.initFields = new FunctionLeekValue(0) {public Object run(AI ai, Object u_this, Object... values) throws LeekRunException {
return null;
}};
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
return null;
}
protected String getAIString() { return "<snippet 7255>";}
protected String[] getErrorFiles() { return new String[] {};}

protected int[] getErrorFilesID() { return new int[] {};}

}
